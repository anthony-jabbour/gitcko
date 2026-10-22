// SPDX-License-Identifier: AGPL-3.0-or-later

//! Built-in defaults, the lowest layer (CONFIGURATION §5–6).

use serde_json::json;

use crate::file::{Items, flatten};
use crate::{Layer, Source};

fn items(defaults: serde_json::Value) -> Items {
    let source = Source {
        layer: Layer::Default,
        file: None,
        line: None,
    };
    flatten(&defaults)
        .into_iter()
        .map(|(k, v)| (k, v, source.clone()))
        .collect()
}

/// `tier` has no default: unset means the organisation's default, or Base locally.
pub(crate) fn repo() -> Items {
    items(json!({
        "strictness": 2,
        "comment_types": ["logic", "syntax", "style"],
        "auto_review": ["open"],
        "review_drafts": false,
        "skip_auto_review": false,
        "exclude_authors": ["dependabot[bot]", "renovate[bot]"],
        "ignore": [
            "*.lock", "package-lock.json", "pnpm-lock.yaml", "yarn.lock", "go.sum",
            "vendor/", "dist/", "*.min.*"
        ],
        "check_run": true,
        "summary_only": false,
        "fix_links": true,
        "hide_footer": false,
        "update_description": false,
        "auto_approve": { "enabled": false, "risk_ceiling": "low" },
        "summary": {
            "overview": { "included": true, "collapsible": false, "default_open": true },
            "files_table": { "included": true, "collapsible": true, "default_open": false },
            "score": { "included": true },
            "diagram": { "included": true, "collapsible": true, "default_open": false },
            "security": { "included": true }
        }
    }))
}

/// Telemetry is off unless the user opts in (ADR-55); `color` unset means automatic.
pub(crate) fn user() -> Items {
    items(json!({
        "telemetry": false,
        "api_url": "https://api.gitcko.dev",
        "web_url": "https://app.gitcko.dev",
        "review": { "output": "auto", "layout": "comments", "context": 15 }
    }))
}
