// SPDX-License-Identifier: AGPL-3.0-or-later

//! Repository text inside a prompt is fenced: wrapped in markers the text can't produce,
//! so it can't close its own block and pose as instructions (SECURITY §17.3, TM-01).
//! The defence doesn't rest on the model obeying the fence: authority (tools, budgets,
//! destinations) stays in code. The fence keeps data and policy visibly apart.

use gitcko_domain::ContentHash;

/// `content` between `⟦untrusted:<tag>⟧ <label>` and `⟦/untrusted:<tag>⟧`. The tag is
/// derived from the content, so the same input renders the same prompt (cassette keys
/// stay stable), and it is re-derived until the content doesn't contain it.
pub fn fence(label: &str, content: &str) -> String {
    let mut salt = 0u32;
    loop {
        let mut bytes = salt.to_le_bytes().to_vec();
        bytes.extend_from_slice(content.as_bytes());
        let tag: String = ContentHash::of(&bytes).to_string()[..16].to_string();
        if !content.contains(&tag) {
            return format!("⟦untrusted:{tag}⟧ {label}\n{content}\n⟦/untrusted:{tag}⟧");
        }
        salt += 1;
    }
}
