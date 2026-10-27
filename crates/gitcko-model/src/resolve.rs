// SPDX-License-Identifier: AGPL-3.0-or-later

//! `[models]` + environment → the bindings a run can use, each with the destination
//! its calls go to. Locality is computed here from the endpoint and never taken from
//! config: a user can't label a remote host "local" (LOCAL_DEPLOYMENT §4).

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, Ipv6Addr};

use gitcko_config::Origin;
use gitcko_protocol::config::{
    DataPolicy, ModelBinding, ModelClass, ModelFamily, ModelProvider, ModelRole, ModelsConfig,
    ReasoningEffort,
};
use serde::Serialize;

use crate::{ResolveError, Secret};

const OPENROUTER_KEY: &str = "OPENROUTER_API_KEY";
const OLLAMA_HOST: &str = "OLLAMA_HOST";
/// Ollama's own default when `OLLAMA_HOST` is unset or has no port.
const OLLAMA_DEFAULT_PORT: u16 = 11434;

/// The class each role uses unless `[models.roles]` overrides it (MODEL_ROUTING §2).
pub fn class_of(role: ModelRole) -> ModelClass {
    use ModelClass::*;
    use ModelRole::*;
    match role {
        RiskClassifier | ReviewLight | Summary | ReReviewGate | Digest => Fast,
        Review | Judge | Refiner | Scorer | Verify | Ask | ReplyAgent | AddressedJudge
        | MemoryLearner => Smart,
        ReviewDeep => Deep,
        Embeddings => Embed,
    }
}

/// What a model can do, from the provider's model metadata (filled in by the adapter),
/// compared against each class's floor (MODEL_ROUTING §3) before a binding is used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Capabilities {
    pub tools: bool,
    pub structured_output: bool,
    pub reasoning_effort: bool,
    pub temperature: bool,
    /// Context window in tokens.
    pub context: u32,
}

impl Capabilities {
    /// The smallest context a class accepts, also Ollama's `num_ctx` default.
    pub fn context_floor(class: ModelClass) -> Option<u32> {
        match class {
            ModelClass::Fast => Some(32_768),
            ModelClass::Smart | ModelClass::Deep => Some(131_072),
            ModelClass::Embed => None,
        }
    }

    /// Whether this model may serve `class` (MODEL_ROUTING §3 capability floor).
    pub fn meets(&self, class: ModelClass) -> bool {
        let context = Capabilities::context_floor(class).is_none_or(|floor| self.context >= floor);
        match class {
            ModelClass::Fast => self.structured_output && context,
            ModelClass::Smart | ModelClass::Deep => self.tools && self.structured_output && context,
            ModelClass::Embed => true,
        }
    }
}

/// How far a call's data travels, least to most. A run that allows `OnDevice` only
/// never sends anywhere else, fallbacks included.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DestinationClass {
    /// This machine: an Ollama endpoint on a loopback address.
    OnDevice,
    /// A provider the user holds the account with (BYOK OpenRouter, a remote Ollama).
    CustomerRemote,
}

/// Where a binding's calls go.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Destination {
    pub class: DestinationClass,
    pub host: String,
    pub port: u16,
}

/// One usable binding: the exact model, where it runs, and its key if it needs one.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedBinding {
    pub name: String,
    pub provider: ModelProvider,
    pub model: String,
    pub family: ModelFamily,
    pub reasoning_effort: Option<ReasoningEffort>,
    /// OpenRouter only; `deny_collection` unless set.
    pub data_policy: Option<DataPolicy>,
    /// Ollama only: the origin requests go to.
    pub endpoint: Option<Origin>,
    /// Ollama only: `num_ctx`.
    pub context: Option<u32>,
    pub destination: Destination,
    /// From the environment; `Debug` prints `<secret>`.
    pub credential: Option<Secret>,
}

/// Every binding of a run and which role uses which.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedModels {
    pub bindings: BTreeMap<String, ResolvedBinding>,
    /// Only roles that have a binding; the rest are `not_configured`.
    pub roles: BTreeMap<ModelRole, String>,
    pub fallbacks: BTreeMap<ModelClass, Vec<String>>,
    /// Model inversion, on unless `[models.inversion] enabled = false`.
    pub inversion: bool,
}

impl ResolvedModels {
    /// The primary binding for `role`, if one is configured.
    pub fn binding_for(&self, role: ModelRole) -> Option<&ResolvedBinding> {
        self.roles
            .get(&role)
            .and_then(|name| self.bindings.get(name))
    }

    /// Fails on the first role in `required` with no binding. Which roles are required
    /// depends on the run (tier, Verify), so the caller passes them.
    pub fn require(&self, required: &[ModelRole]) -> Result<(), ResolveError> {
        match required.iter().find(|r| self.binding_for(**r).is_none()) {
            Some(role) => Err(ResolveError::MissingBinding(*role)),
            None => Ok(()),
        }
    }
}

/// Resolves `[models]` against the environment (`OPENROUTER_API_KEY`, `OLLAMA_HOST`).
pub fn resolve(
    models: &ModelsConfig,
    env: &BTreeMap<String, String>,
) -> Result<ResolvedModels, ResolveError> {
    let configured = models.bindings.clone().unwrap_or_default();
    let unknown = |key: String| ResolveError::UnknownBinding { key };
    let mut roles = BTreeMap::new();
    for role in all_roles() {
        match models.roles.as_ref().and_then(|r| r.get(&role)) {
            Some(name) if !configured.contains_key(name) => {
                return Err(unknown(format!("models.roles.{}", crate::wire_id(&role))));
            }
            Some(name) => {
                roles.insert(role, name.clone());
            }
            // Unset: the binding named after the role's class, if there is one.
            None => {
                let class = crate::wire_id(&class_of(role));
                if configured.contains_key(&class) {
                    roles.insert(role, class);
                }
            }
        }
    }
    let fallbacks = models.fallbacks.clone().unwrap_or_default();
    for (class, names) in &fallbacks {
        if !names.iter().all(|n| configured.contains_key(n)) {
            return Err(unknown(format!(
                "models.fallbacks.{}",
                crate::wire_id(class)
            )));
        }
    }
    let mut bindings = BTreeMap::new();
    for (name, binding) in configured {
        // Every class that can reach this binding, as a primary or a fallback.
        let classes: Vec<ModelClass> = roles
            .iter()
            .filter(|(_, n)| **n == name)
            .map(|(role, _)| class_of(*role))
            .chain(
                fallbacks
                    .iter()
                    .filter(|(_, ns)| ns.contains(&name))
                    .map(|(c, _)| *c),
            )
            .collect();
        let resolved = match binding.provider {
            ModelProvider::Openrouter => openrouter(name.clone(), binding, env)?,
            ModelProvider::Ollama => ollama(name.clone(), binding, &classes, env)?,
        };
        bindings.insert(name, resolved);
    }
    Ok(ResolvedModels {
        bindings,
        roles,
        fallbacks,
        inversion: models
            .inversion
            .as_ref()
            .and_then(|i| i.enabled)
            .unwrap_or(true),
    })
}

fn all_roles() -> impl Iterator<Item = ModelRole> {
    use ModelRole::*;
    [
        RiskClassifier,
        ReviewLight,
        Review,
        ReviewDeep,
        Judge,
        Refiner,
        Summary,
        Scorer,
        Verify,
        Ask,
        Embeddings,
        ReplyAgent,
        ReReviewGate,
        AddressedJudge,
        MemoryLearner,
        Digest,
    ]
    .into_iter()
}

fn openrouter(
    name: String,
    binding: ModelBinding,
    env: &BTreeMap<String, String>,
) -> Result<ResolvedBinding, ResolveError> {
    let key = env.get(OPENROUTER_KEY).filter(|k| !k.is_empty()).ok_or(
        ResolveError::MissingCredential {
            var: OPENROUTER_KEY,
            provider: "openrouter",
        },
    )?;
    // OpenRouter ids are `vendor/model`; the vendor names the family.
    let vendor = binding.model.split_once('/').map_or("", |(v, _)| v);
    let family = binding.family.unwrap_or(match vendor {
        "anthropic" => ModelFamily::Anthropic,
        "openai" => ModelFamily::Openai,
        "google" => ModelFamily::Google,
        _ => ModelFamily::OpenWeight,
    });
    Ok(ResolvedBinding {
        name,
        provider: ModelProvider::Openrouter,
        model: binding.model,
        family,
        reasoning_effort: binding.reasoning_effort,
        data_policy: Some(binding.data_policy.unwrap_or(DataPolicy::DenyCollection)),
        endpoint: None,
        context: None,
        destination: Destination {
            class: DestinationClass::CustomerRemote,
            host: "openrouter.ai".into(),
            port: 443,
        },
        credential: Some(Secret::new(key.clone())),
    })
}

fn ollama(
    name: String,
    binding: ModelBinding,
    classes: &[ModelClass],
    env: &BTreeMap<String, String>,
) -> Result<ResolvedBinding, ResolveError> {
    let origin = match (
        &binding.endpoint,
        env.get(OLLAMA_HOST).filter(|h| !h.is_empty()),
    ) {
        (Some(url), _) => Origin::parse(url)
            .filter(plain_http_only_on_loopback)
            .ok_or_else(|| ResolveError::BadEndpoint(format!("models.bindings.{name}.endpoint")))?,
        (None, Some(host)) => {
            ollama_host(host).ok_or_else(|| ResolveError::BadEndpoint(OLLAMA_HOST.into()))?
        }
        (None, None) => Origin {
            https: false,
            host: "127.0.0.1".into(),
            port: Some(OLLAMA_DEFAULT_PORT),
        },
    };
    let port = origin.port.unwrap_or(if origin.https { 443 } else { 80 });
    let class = if is_loopback_host(&origin.host) {
        DestinationClass::OnDevice
    } else {
        DestinationClass::CustomerRemote
    };
    let floor = classes
        .iter()
        .filter_map(|c| Capabilities::context_floor(*c))
        .max();
    Ok(ResolvedBinding {
        name,
        provider: ModelProvider::Ollama,
        model: binding.model,
        family: binding.family.unwrap_or(ModelFamily::OpenWeight),
        reasoning_effort: binding.reasoning_effort,
        data_policy: None,
        context: binding.context.or(floor),
        destination: Destination {
            class,
            host: origin.host.clone(),
            port,
        },
        endpoint: Some(origin),
        credential: None,
    })
}

/// `OLLAMA_HOST` the way Ollama reads it: `host`, `host:port` or a full URL; plain
/// http is assumed and the port defaults to 11434.
fn ollama_host(value: &str) -> Option<Origin> {
    let url = if value.contains("://") {
        value.to_string()
    } else {
        format!("http://{value}")
    };
    let mut origin = Origin::parse(&url).filter(plain_http_only_on_loopback)?;
    if origin.port.is_none() && !origin.https {
        origin.port = Some(OLLAMA_DEFAULT_PORT);
    }
    Some(origin)
}

/// Prompts hold source code, so they cross a network only over TLS (CONFIGURATION §6).
fn plain_http_only_on_loopback(origin: &Origin) -> bool {
    origin.https || is_loopback_host(&origin.host)
}

/// A host that can only be this machine: `localhost`, `127.0.0.0/8` or `[::1]`.
/// Anything else, `0.0.0.0` and look-alike names included, counts as remote. The
/// Ollama adapter also checks that `localhost` resolves only to loopback and pins the
/// connection to those addresses, so DNS can't move it.
pub fn is_loopback_host(host: &str) -> bool {
    if host == "localhost" {
        return true;
    }
    if let Some(v6) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        return v6.parse::<Ipv6Addr>().is_ok_and(|ip| ip.is_loopback());
    }
    // Only a dotted-quad literal: `127.0.0.1.nip.io` doesn't parse, so it's remote.
    host.parse::<Ipv4Addr>().is_ok_and(|ip| ip.is_loopback())
}

/// One destination the run may send data to, and the roles whose primary binding
/// goes there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Disclosed {
    pub provider: ModelProvider,
    pub host: String,
    pub port: u16,
    pub class: DestinationClass,
    pub roles: Vec<ModelRole>,
}

/// What the review header discloses (LOCAL_DEPLOYMENT §4.3): every destination, and
/// the most remote one, which decides the headline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Disclosure {
    pub destinations: Vec<Disclosed>,
    pub most_remote: Option<DestinationClass>,
}

/// Built from every binding, not only the primaries: a fallback can send data too.
pub fn disclose(models: &ResolvedModels) -> Disclosure {
    let mut destinations: Vec<Disclosed> = Vec::new();
    for binding in models.bindings.values() {
        let d = &binding.destination;
        let roles = models
            .roles
            .iter()
            .filter(|(_, name)| **name == binding.name)
            .map(|(role, _)| *role);
        match destinations
            .iter_mut()
            .find(|x| x.provider == binding.provider && x.host == d.host && x.port == d.port)
        {
            Some(existing) => existing.roles.extend(roles),
            None => destinations.push(Disclosed {
                provider: binding.provider,
                host: d.host.clone(),
                port: d.port,
                class: d.class,
                roles: roles.collect(),
            }),
        }
    }
    for d in &mut destinations {
        d.roles.sort();
    }
    let most_remote = destinations.iter().map(|d| d.class).max();
    Disclosure {
        destinations,
        most_remote,
    }
}
