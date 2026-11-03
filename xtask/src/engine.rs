// SPDX-License-Identifier: AGPL-3.0-or-later

//! `gitcko-review` behind the harness's [`Engine`] seam. Replay gets cassettes and a
//! placeholder key, so no real key is read and no adapter is built; only record and live
//! can reach a provider.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use gitcko_cassette::Cassette;
use gitcko_git::Repo;
use gitcko_model::{ModelProvider, ResolvedModels, Router, resolve};
use gitcko_model_openai::OpenRouter;
use gitcko_protocol::config::{ModelProvider as Provider, ModelRole, ModelsConfig};
use gitcko_review::{ReviewInput, base_run, review};
use gitcko_test_support::workspace_root as root;
use serde::Deserialize;

use crate::Result;
use crate::eval::{Case, Engine, Models, Outcome};
use crate::fault::Injector;

const MODELS: &str = "evals/models.toml";
const CASSETTES: &str = "tests/cassettes";
const API_KEY: &str = "OPENROUTER_API_KEY";
/// Marks the ids `evals/models.toml` ships with, which no provider serves.
const UNCHOSEN: &str = "choose/";

#[derive(Deserialize)]
struct ModelsFile {
    models: ModelsConfig,
}

/// The real engine.
pub struct ReviewEngine;

/// `text` as a `[models]` table, with `binding` serving the roles it was named for.
fn models_config(text: &str, binding: Option<&str>) -> Result<ModelsConfig> {
    let file: ModelsFile = toml::from_str(text).map_err(|_| format!("{MODELS} doesn't parse"))?;
    let mut models = file.models;
    let Some(binding) = binding else {
        return Ok(models);
    };
    if !models
        .bindings
        .as_ref()
        .is_some_and(|b| b.contains_key(binding))
    {
        return Err(format!("{binding} isn't a binding in {MODELS}"));
    }
    let roles = models.roles.get_or_insert_with(BTreeMap::new);
    roles.insert(ModelRole::Review, binding.to_string());
    roles.insert(ModelRole::RiskClassifier, binding.to_string());
    Ok(models)
}

/// One provider per binding: cassettes on replay, the adapter otherwise. A case's fault
/// wraps outside either, so a faulted call is never sent and never recorded; under
/// record the cassette holds exactly the calls a replay of the faulted case makes.
fn providers(
    resolved: &ResolvedModels,
    models: &Models,
    env: &BTreeMap<String, String>,
    fault: Option<&Injector>,
) -> Result<BTreeMap<String, Arc<dyn ModelProvider>>> {
    let mut out: BTreeMap<String, Arc<dyn ModelProvider>> = BTreeMap::new();
    for (name, binding) in &resolved.bindings {
        let provider: Arc<dyn ModelProvider> = match models {
            Models::Replay | Models::ReplayAs(_) => {
                Arc::new(Cassette::replay(root().join(CASSETTES), binding))
            }
            Models::Record(_) | Models::Live(_) => {
                if binding.provider != Provider::Openrouter {
                    return Err(format!("{name}: evals run OpenRouter bindings"));
                }
                if binding.model.starts_with(UNCHOSEN) {
                    return Err(format!("{name}: set a pinned model id in {MODELS}"));
                }
                let adapter = OpenRouter::new(binding.clone()).map_err(|e| e.to_string())?;
                match models {
                    Models::Record(_) => Arc::new(
                        Cassette::record(root().join(CASSETTES), binding, Arc::new(adapter), env)
                            .map_err(|e| e.to_string())?,
                    ),
                    _ => Arc::new(adapter),
                }
            }
        };
        let provider = match fault {
            Some(fault) => fault.wrap(provider),
            None => provider,
        };
        out.insert(name.clone(), provider);
    }
    Ok(out)
}

fn router(models: &Models, fault: Option<&Injector>) -> Result<Router> {
    let text = std::fs::read_to_string(root().join(MODELS))
        .map_err(|_| format!("{MODELS} can't be read"))?;
    let binding = match models {
        Models::Replay => None,
        Models::ReplayAs(binding) | Models::Record(binding) | Models::Live(binding) => {
            Some(binding.as_str())
        }
    };
    let config = models_config(&text, binding)?;
    // Replay never looks for a key, not even in `.env`.
    let env = if models.is_replay() {
        let mut env: BTreeMap<String, String> = std::env::vars().collect();
        env.insert(API_KEY.to_string(), "replay-needs-no-key".to_string());
        env
    } else {
        crate::dotenv::load(&root())?
    };
    let resolved = resolve(&config, &env).map_err(|e| e.to_string())?;
    let providers = providers(&resolved, models, &env, fault)?;
    Ok(Router::new(resolved, providers))
}

/// Runs the review of `repo` through `router`; `remote` is whether a live provider is behind it.
fn execute(
    case: &Case,
    repo: &Path,
    router: &Router,
    injector: Option<&Injector>,
    remote: bool,
) -> Result<Outcome> {
    let repo = Repo::open(repo).map_err(|e| e.to_string())?;
    let at = |rev| repo.resolve(rev).map_err(|e| e.to_string());
    // Results carry no events here, so their times are never read.
    let now = || "1970-01-01T00:00:00Z".to_string();
    let input = ReviewInput {
        repo: &repo,
        base: at("base")?,
        head: at("main")?,
        now: &now,
    };
    let mut run = base_run(format!("rv_{}", case.name), remote);
    if let Some(injector) = injector {
        injector.arm(&mut run);
    }
    // A replayed fault has no socket to wait on, so retries and the deadline run on a
    // paused clock instead of costing their time.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(injector.is_some() && !remote)
        .build()
        .map_err(|e| e.to_string())?;
    let finished = runtime.block_on(review(input, router, run));
    if injector.is_some_and(|i| !i.fired()) {
        return Err(format!(
            "{}: the run ended before its fault was reached, so the case tested nothing; \
             when recording, an earlier call may have outrun the injected deadline: record again",
            case.id()
        ));
    }
    let cost_usd = finished.review.usage.cost_usd;
    Ok(Outcome {
        review: finished.review,
        cost_usd,
    })
}

impl Engine for ReviewEngine {
    fn review(&self, case: &Case, repo: &Path, models: &Models) -> Result<Outcome> {
        let injector = case.expect.fault.as_ref().map(Injector::new);
        let router = router(models, injector.as_ref())?;
        execute(case, repo, &router, injector.as_ref(), !models.is_replay())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::{Suite, load};
    use gitcko_protocol::review::ReviewStatus;

    fn committed() -> String {
        std::fs::read_to_string(root().join(MODELS)).unwrap()
    }

    fn fake_env() -> BTreeMap<String, String> {
        BTreeMap::from([(API_KEY.to_string(), format!("{}-{}", "not", "a-key"))])
    }

    fn roles(config: &ModelsConfig) -> &BTreeMap<ModelRole, String> {
        config.roles.as_ref().unwrap()
    }

    #[test]
    fn the_committed_models_file_resolves_with_a_binding_per_required_role() {
        let config = models_config(&committed(), None).unwrap();
        let resolved = resolve(&config, &fake_env()).unwrap();
        for role in [
            ModelRole::Review,
            ModelRole::RiskClassifier,
            ModelRole::Judge,
        ] {
            assert!(resolved.roles.contains_key(&role), "{role:?}");
        }
    }

    #[test]
    fn a_named_binding_serves_review_and_risk_but_not_the_judge() {
        let config = models_config(&committed(), Some("judge")).unwrap();
        assert_eq!(roles(&config)[&ModelRole::Review], "judge");
        assert_eq!(roles(&config)[&ModelRole::RiskClassifier], "judge");
        let config = models_config(&committed(), Some("review")).unwrap();
        assert_eq!(roles(&config)[&ModelRole::Judge], "judge");
    }

    #[test]
    fn a_binding_the_file_does_not_have_is_refused() {
        let err = models_config(&committed(), Some("nope")).err().unwrap();
        assert!(err.contains("nope"), "{err}");
    }

    #[test]
    fn every_candidate_can_serve_the_review_roles() {
        for binding in ["review", "review-sonnet"] {
            let config = models_config(&committed(), Some(binding)).unwrap();
            let resolved = resolve(&config, &fake_env()).unwrap();
            let record = Models::Record(binding.into());
            assert!(
                providers(&resolved, &record, &fake_env(), None).is_ok(),
                "{binding}"
            );
        }
    }

    #[test]
    fn a_placeholder_id_never_reaches_a_provider() {
        // The committed file with its ids put back to the placeholders it shipped with.
        let unchosen: String = committed()
            .lines()
            .map(|l| match l.starts_with("model = ") {
                true => format!("model = \"{UNCHOSEN}a-pinned-model\"\n"),
                false => format!("{l}\n"),
            })
            .collect();
        let config = models_config(&unchosen, None).unwrap();
        let resolved = resolve(&config, &fake_env()).unwrap();
        for models in [
            Models::Live("review".into()),
            Models::Record("review".into()),
        ] {
            let err = providers(&resolved, &models, &fake_env(), None)
                .err()
                .unwrap();
            assert!(err.contains("pinned model id"), "{err}");
        }
        // Positive controls: replay builds, since it reaches no provider at all, and the
        // committed ids are accepted for a recording.
        assert!(providers(&resolved, &Models::Replay, &fake_env(), None).is_ok());
        let chosen = resolve(&models_config(&committed(), None).unwrap(), &fake_env()).unwrap();
        assert!(providers(&chosen, &Models::Record("review".into()), &fake_env(), None).is_ok());
    }

    /// The review of a Suite F case over an inner provider that answers nothing (every
    /// call it gets fails), so only cases whose faulted call is the first review call
    /// can finish here; the rest need recordings. Without `faulted`, nothing is injected.
    fn run_fake(name: &str, faulted: bool) -> (Case, Outcome) {
        let (case, outcome) = try_fake(name, faulted);
        (case, outcome.unwrap())
    }

    fn try_fake(name: &str, faulted: bool) -> (Case, Result<Outcome>) {
        let case = load(&root().join(crate::eval::ROOT), Suite::F)
            .unwrap()
            .into_iter()
            .find(|c| c.name == name)
            .unwrap();
        let injector = Injector::new(case.expect.fault.as_ref().unwrap());
        let resolved = resolve(&models_config(&committed(), None).unwrap(), &fake_env()).unwrap();
        let inner: Arc<dyn ModelProvider> = Arc::new(gitcko_model::FakeProvider::new(vec![]));
        let providers = resolved
            .bindings
            .keys()
            .map(|n| {
                let p = if faulted {
                    injector.wrap(inner.clone())
                } else {
                    inner.clone()
                };
                (n.clone(), p)
            })
            .collect();
        let router = Router::new(resolved, providers);
        let script = std::path::absolute(case.dir.join("make.sh")).unwrap();
        let repo = gitcko_test_support::script_repo(&script, &case.name);
        let armed = faulted.then_some(&injector);
        let outcome = execute(&case, repo.path(), &router, armed, false);
        (case, outcome)
    }

    /// Here the inner fails the first review call, so the run ends before call 2, where
    /// the fault waits. Under record the same happens when an earlier call outruns the
    /// injected deadline: the result could still look right, with nothing recorded.
    #[test]
    fn a_fault_the_run_never_reached_fails_the_case() {
        let (_, got) = try_fake("deadline_investigate", true);
        let err = got.err().unwrap();
        assert!(err.contains("before its fault was reached"), "{err}");
        // Positive control: a fault at the first call is reached.
        assert!(try_fake("deadline_plan", true).1.is_ok());
    }

    /// Judge fault cases are left out until the judge lands, and `malformed` and the
    /// risk-classifier cases need a recorded reply for the call the fault spares.
    #[test]
    fn faults_at_the_first_review_call_end_the_run_as_the_case_expects() {
        for name in [
            "timeout_plan",
            "unavailable_plan",
            "refusal_plan",
            "tokens_plan",
            "deadline_plan",
            "invalid_repair_plan",
            "cancel_plan",
        ] {
            let started = std::time::Instant::now();
            let (case, got) = run_fake(name, true);
            // On the paused clock the deadline and the backoffs cost no real time.
            assert!(
                started.elapsed().as_secs() < 10,
                "{name}: {:?}",
                started.elapsed()
            );
            assert_eq!(got.review.status, case.expect.status, "{name}");
            let mut reasons = got.review.reason_codes.clone();
            reasons.sort();
            let mut want = case.expect.reason_codes.clone();
            // An invalid plan still runs the graph-seeded hypotheses (`report` calls the
            // changed function); here the fake answers nothing, so that call fails too.
            if name == "invalid_repair_plan" {
                want.extend(["provider_failure".into(), "review_failed".into()]);
            }
            want.sort();
            assert_eq!(reasons, want, "{name}");
            // Positive control: with nothing injected the run is the baseline failure, so
            // every other result above came from the fault.
            let (_, plain) = run_fake(name, false);
            assert_eq!(plain.review.status, ReviewStatus::Failed);
            assert_eq!(
                plain.review.reason_codes,
                ["provider_failure", "review_failed"]
            );
        }
    }

    /// The first provider `providers` builds for a replay with a timeout injected at `at_call`.
    fn faulted_replay_call(
        at_call: u32,
    ) -> Vec<std::result::Result<gitcko_model::Event, gitcko_model::ProviderError>> {
        use crate::eval::{Fault, FaultKind, FaultRole};
        use futures_util::StreamExt;
        let fault = Fault {
            kind: FaultKind::Timeout,
            role: FaultRole::Review,
            at_call,
        };
        let injector = Injector::new(&fault);
        let resolved = resolve(&models_config(&committed(), None).unwrap(), &fake_env()).unwrap();
        let built = providers(&resolved, &Models::Replay, &fake_env(), Some(&injector)).unwrap();
        let request = gitcko_model::Request {
            role: ModelRole::Review,
            prompt: gitcko_model::PromptRef {
                id: "review".into(),
                version: "1".into(),
            },
            messages: vec![],
            tools: vec![],
            response_format: None,
            max_output: 16,
            reasoning_effort: None,
            temperature: None,
        };
        let stream = built
            .values()
            .next()
            .unwrap()
            .stream(request, Default::default());
        futures_util::FutureExt::now_or_never(stream.collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn a_fault_wraps_the_replay_providers_so_a_faulted_call_reads_no_cassette() {
        let items = faulted_replay_call(1);
        assert!(
            items[0]
                .as_ref()
                .is_err_and(|e| matches!(e.kind, gitcko_model::ErrorKind::Timeout(_)))
        );
    }

    /// Also the proof that replay never falls back to a live call: a request nobody
    /// recorded fails loudly. (Real cases can't show it once their cassettes exist.)
    #[test]
    #[should_panic(expected = "no cassette")]
    fn a_call_before_the_fault_still_reads_its_cassette() {
        let _ = faulted_replay_call(2);
    }
}
