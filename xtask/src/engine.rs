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

/// One provider per binding: cassettes on replay, the adapter otherwise.
fn providers(
    resolved: &ResolvedModels,
    models: &Models,
    env: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, Arc<dyn ModelProvider>>> {
    let mut out: BTreeMap<String, Arc<dyn ModelProvider>> = BTreeMap::new();
    for (name, binding) in &resolved.bindings {
        let provider: Arc<dyn ModelProvider> = match models {
            Models::Replay => Arc::new(Cassette::replay(root().join(CASSETTES), binding)),
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
        out.insert(name.clone(), provider);
    }
    Ok(out)
}

fn router(models: &Models) -> Result<Router> {
    let text = std::fs::read_to_string(root().join(MODELS))
        .map_err(|_| format!("{MODELS} can't be read"))?;
    let binding = match models {
        Models::Replay => None,
        Models::Record(binding) | Models::Live(binding) => Some(binding.as_str()),
    };
    let config = models_config(&text, binding)?;
    let mut env: BTreeMap<String, String> = std::env::vars().collect();
    if *models == Models::Replay {
        env.insert(API_KEY.to_string(), "replay-needs-no-key".to_string());
    }
    let resolved = resolve(&config, &env).map_err(|e| e.to_string())?;
    let providers = providers(&resolved, models, &env)?;
    Ok(Router::new(resolved, providers))
}

impl Engine for ReviewEngine {
    fn review(&self, case: &Case, repo: &Path, models: &Models) -> Result<Outcome> {
        // ponytail: scripted faults arrive with their provider wrapper (batch B5b); until
        // then a fault case can't be run honestly, so it isn't run.
        if case.expect.fault.is_some() {
            return Err(format!("{}: fault injection isn't built yet", case.id()));
        }
        let router = router(models)?;
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
        let remote = *models != Models::Replay;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        let finished = runtime.block_on(async {
            review(
                input,
                &router,
                base_run(format!("rv_{}", case.name), remote),
            )
            .await
        });
        let cost_usd = finished.review.usage.cost_usd;
        Ok(Outcome {
            review: finished.review,
            cost_usd,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::{Suite, load, run_case};

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
    fn the_shipped_placeholder_ids_never_reach_a_provider() {
        let config = models_config(&committed(), None).unwrap();
        let resolved = resolve(&config, &fake_env()).unwrap();
        for models in [
            Models::Live("review".into()),
            Models::Record("review".into()),
        ] {
            let err = providers(&resolved, &models, &fake_env()).err().unwrap();
            assert!(err.contains("pinned model id"), "{err}");
        }
        // Positive control: replay builds, since it reaches no provider at all.
        assert!(providers(&resolved, &Models::Replay, &fake_env()).is_ok());
    }

    #[test]
    fn a_case_with_a_fault_is_refused_rather_than_run_without_it() {
        let cases = load(&root().join(crate::eval::ROOT), Suite::F).unwrap();
        let err = run_case(&ReviewEngine, &cases[0], &Models::Replay)
            .err()
            .unwrap();
        assert!(err.contains("fault injection"), "{err}");
    }

    #[test]
    #[should_panic(expected = "no cassette")]
    fn replay_without_a_cassette_fails_loudly() {
        let cases = load(&root().join(crate::eval::ROOT), Suite::A).unwrap();
        let case = cases.iter().find(|c| c.expect.fault.is_none()).unwrap();
        let _ = run_case(&ReviewEngine, case, &Models::Replay);
    }
}
