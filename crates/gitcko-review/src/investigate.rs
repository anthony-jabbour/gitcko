// SPDX-License-Identifier: AGPL-3.0-or-later

//! The bounded investigator loop (REVIEW_ENGINE §10–11): one hypothesis at a time, one
//! tool exchange per turn, until the model reports an outcome or a bound is hit. Tools
//! are synchronous and bounded, so the token is checked around each call instead of
//! running them on a blocking pool; a result arriving after cancel isn't used.

use std::collections::HashMap;

use gitcko_model::{
    CallError, CallRecord, ErrorKind, FinishReason, Message, Request, Response, Router, Run,
};
use gitcko_prompts::{
    Candidate, Checked, InvestigationOutput, Outcome, investigate, parse, repair, tool_result,
};
use gitcko_protocol::config::ModelRole;
use gitcko_tools::{Ledger, Tools, definitions};
use tokio::time::Instant;

use crate::complete::{Completeness, reason};
use crate::events::Events;
use crate::plan::Task;
use crate::snapshot::Snapshot;
use crate::{ACTIONS_PER_HYPOTHESIS, ACTIONS_PER_RUN, NO_PROGRESS_TURNS, RESERVE_PERCENT};

/// What asking the model for a typed answer came to.
pub enum Asked<T> {
    Parsed(T),
    /// No answer: the run stopped or every binding failed.
    Failed(CallError),
    /// The provider's safety system ended the reply.
    Refused,
    /// The reply and its one repair were both refused by the parser.
    Invalid,
}

/// Every model call of the run, in order, through the router.
pub struct Calls<'a, 'e> {
    router: &'a Router,
    run: &'a mut Run,
    pub events: &'a mut Events<'e>,
    pub records: Vec<CallRecord>,
}

impl<'a, 'e> Calls<'a, 'e> {
    pub fn new(router: &'a Router, run: &'a mut Run, events: &'a mut Events<'e>) -> Calls<'a, 'e> {
        Calls {
            router,
            run,
            events,
            records: Vec::new(),
        }
    }

    pub fn canceled(&self) -> bool {
        self.run.cancel.is_cancelled()
    }

    /// Which ceiling spend has reached the share kept for validation, the judge and
    /// assembly, if any.
    fn reserve_reached(&self) -> Option<&'static str> {
        let spent = self.run.budget.spent();
        let limits = self.run.budget.limits();
        let share = 100 - RESERVE_PERCENT;
        let over =
            |spent: u64, limit: u64| spent.saturating_mul(100) >= limit.saturating_mul(share);
        if over(spent.input_tokens, limits.input_tokens)
            || over(spent.output_tokens, limits.output_tokens)
        {
            return Some(reason::TOKENS);
        }
        let cost_over = limits
            .cost_usd
            .is_some_and(|limit| spent.cost_usd * 100.0 >= limit * share as f64);
        cost_over.then_some(reason::COST)
    }

    pub async fn call(&mut self, step: &str, request: Request) -> Result<Response, CallError> {
        let outcome = self.router.call(self.run, request).await;
        for record in &outcome.records {
            self.events.model_call(step, record);
        }
        self.records.extend(outcome.records);
        outcome.result
    }

    /// One call parsed as `T`, with the one repair a bad reply gets (MODEL_ROUTING §7).
    pub async fn ask<T: Checked>(&mut self, step: &str, request: Request) -> Asked<T> {
        let first = match self.call(step, request.clone()).await {
            Ok(response) => response,
            Err(error) => return Asked::Failed(error),
        };
        self.finish_typed(step, &request, first).await
    }

    /// Parses a reply already received, repairing once.
    async fn finish_typed<T: Checked>(
        &mut self,
        step: &str,
        request: &Request,
        reply: Response,
    ) -> Asked<T> {
        if reply.finish == FinishReason::ContentFilter {
            return Asked::Refused;
        }
        let problems = match parse::<T>(&reply.text) {
            Ok(parsed) => return Asked::Parsed(parsed),
            Err(error) => error.problems,
        };
        let again = repair(request, &reply.text, &problems);
        let reply = match self.call(step, again).await {
            Ok(response) => response,
            Err(error) => return Asked::Failed(error),
        };
        if reply.finish == FinishReason::ContentFilter {
            return Asked::Refused;
        }
        match parse::<T>(&reply.text) {
            Ok(parsed) => Asked::Parsed(parsed),
            Err(_) => Asked::Invalid,
        }
    }
}

/// What investigation produced.
#[derive(Default)]
pub struct Findings {
    pub candidates: Vec<(Task, Candidate)>,
    /// Refuted hypotheses, with why: the result's `cleared_risks`.
    pub refuted: Vec<(Task, String)>,
    pub ledger: Ledger,
    pub supported: u64,
    pub inconclusive: u64,
    pub blocked: u64,
    pub deferred: u64,
    pub tool_actions: u64,
    /// Candidates dropped for citing evidence no tool returned.
    pub dropped: u64,
}

impl Findings {
    pub fn counts(&self) -> Vec<(&'static str, u64)> {
        vec![
            ("supported", self.supported),
            ("refuted", self.refuted.len() as u64),
            ("inconclusive", self.inconclusive),
            ("blocked", self.blocked),
            ("deferred", self.deferred),
            ("candidates", self.candidates.len() as u64),
            ("dropped", self.dropped),
            ("tool_actions", self.tool_actions),
        ]
    }
}

/// How one investigation ended.
enum Ended {
    Reported(InvestigationOutput),
    /// A bound stopped it.
    Inconclusive,
    /// Its model output couldn't be used.
    Blocked,
    /// The run is over; the reason is on the ledger.
    Stopped,
}

/// Runs each hypothesis in order until they're done or a bound stops admitting them.
pub async fn run_all(
    calls: &mut Calls<'_, '_>,
    snapshot: &Snapshot<'_>,
    tasks: Vec<Task>,
    until: Option<Instant>,
    ledger: &mut Completeness,
) -> Findings {
    let mut tools = Tools::new(
        snapshot.repo,
        &snapshot.index,
        &snapshot.change,
        snapshot.base,
        snapshot.head,
        snapshot.tool_limits(),
    );
    let mut found = Findings::default();
    let mut actions_left = ACTIONS_PER_RUN;
    let mut queue = tasks.into_iter();
    while let Some(task) = queue.next() {
        let past_deadline = until.is_some_and(|u| Instant::now() >= u);
        let reserve = calls.reserve_reached();
        if actions_left == 0 {
            // Not a budget: the review simply saw less than it set out to.
            found.deferred += 1 + queue.len() as u64;
            ledger.reduced(reason::TRUNCATED);
            break;
        }
        if past_deadline || reserve.is_some() {
            // Not admitted: say so, so a short list never reads as a full one.
            found.deferred += 1 + queue.len() as u64;
            ledger.add(reason::BUDGET_EXHAUSTED);
            ledger.add(if past_deadline {
                reason::DEADLINE
            } else {
                reserve.unwrap_or(reason::TOKENS)
            });
            break;
        }
        match investigate_one(
            calls,
            snapshot,
            &task,
            &mut tools,
            &mut actions_left,
            &mut found,
            ledger,
        )
        .await
        {
            Ended::Reported(output) => record(output, task, &tools, snapshot, &mut found),
            Ended::Inconclusive => found.inconclusive += 1,
            Ended::Blocked => found.blocked += 1,
            Ended::Stopped => {
                found.deferred += queue.len() as u64;
                break;
            }
        }
    }
    found.ledger = tools.ledger().clone();
    found
}

fn record(
    output: InvestigationOutput,
    task: Task,
    tools: &Tools<'_>,
    snapshot: &Snapshot<'_>,
    found: &mut Findings,
) {
    let allowed = snapshot.investigated();
    match output.outcome {
        Outcome::Supported => {
            found.supported += 1;
            for candidate in output.candidates {
                // A claim must rest on lines a tool actually returned in this run, and be
                // about a file this review may speak of: never a held-back one.
                let grounded = allowed.iter().any(|a| a.as_str() == candidate.path)
                    && !candidate.evidence.is_empty()
                    && candidate
                        .evidence
                        .iter()
                        .all(|id| tools.ledger().get(id).is_some());
                if grounded {
                    found.candidates.push((task.clone(), candidate));
                } else {
                    found.dropped += 1;
                }
            }
        }
        Outcome::Refuted => found.refuted.push((task, output.reason)),
        Outcome::Inconclusive => found.inconclusive += 1,
        Outcome::Blocked => found.blocked += 1,
    }
}

async fn investigate_one(
    calls: &mut Calls<'_, '_>,
    snapshot: &Snapshot<'_>,
    task: &Task,
    tools: &mut Tools<'_>,
    actions_left: &mut u32,
    found: &mut Findings,
    ledger: &mut Completeness,
) -> Ended {
    let path = gitcko_domain::RepoPath::parse(&task.path).ok();
    let diff = path.map(|p| snapshot.file_diff(&p).0).unwrap_or_default();
    let hypothesis = format!("{}\nFile: {}\n{}", task.title, task.path, task.claim);
    let mut request = investigate(&hypothesis, &diff, definitions());
    let mut cache: HashMap<(String, String), String> = HashMap::new();
    let mut actions = 0;
    let mut no_progress = 0;
    loop {
        let reply = match calls.call("investigate", request.clone()).await {
            Ok(reply) => reply,
            Err(error) => {
                ledger.call_failed(ModelRole::Review, &error, false);
                return Ended::Stopped;
            }
        };
        if reply.finish == FinishReason::ContentFilter {
            refuse(ledger);
            return Ended::Stopped;
        }
        // Told to answer and still calling: nothing more will come of this one.
        if request.tools.is_empty() && !reply.tool_calls.is_empty() {
            return Ended::Inconclusive;
        }
        if reply.tool_calls.is_empty() {
            return match calls
                .finish_typed::<InvestigationOutput>("investigate", &request, reply)
                .await
            {
                Asked::Parsed(output) => Ended::Reported(output),
                Asked::Failed(error) => {
                    ledger.call_failed(ModelRole::Review, &error, false);
                    Ended::Stopped
                }
                Asked::Refused => {
                    refuse(ledger);
                    Ended::Stopped
                }
                Asked::Invalid => {
                    ledger.add(reason::INVALID_MODEL_OUTPUT);
                    Ended::Blocked
                }
            };
        }
        request.messages.push(Message::Assistant {
            text: reply.text.clone(),
            tool_calls: reply.tool_calls.clone(),
        });
        let mut progressed = false;
        for call in &reply.tool_calls {
            if calls.canceled() {
                ledger.call_failed(ModelRole::Review, &CallError::Canceled, false);
                return Ended::Stopped;
            }
            let key = (call.name.clone(), call.arguments.clone());
            let text = match cache.get(&key) {
                // The same call again gets the same answer and costs a turn of progress.
                Some(text) => text.clone(),
                // One reply can ask for any number of calls; the caps hold within it.
                None if actions >= ACTIONS_PER_HYPOTHESIS || *actions_left == 0 => {
                    r#"{"status":"action_limit"}"#.to_string()
                }
                None => {
                    progressed = true;
                    actions += 1;
                    *actions_left = actions_left.saturating_sub(1);
                    found.tool_actions += 1;
                    let text = tools.run_raw(&call.name, &call.arguments).to_message();
                    cache.insert(key, text.clone());
                    text
                }
            };
            request.messages.push(tool_result(&call.id, &text));
        }
        no_progress = if progressed { 0 } else { no_progress + 1 };
        if no_progress >= NO_PROGRESS_TURNS {
            return Ended::Inconclusive;
        }
        // Out of actions or into the reserve: the model must answer from what it has.
        if actions >= ACTIONS_PER_HYPOTHESIS
            || *actions_left == 0
            || calls.reserve_reached().is_some()
        {
            request.tools = Vec::new();
        }
    }
}

fn refuse(ledger: &mut Completeness) {
    let refused = gitcko_model::ProviderError::new(ErrorKind::ContentPolicy);
    ledger.call_failed(
        ModelRole::Review,
        &CallError::ProviderFailed(Some(refused)),
        false,
    );
}
