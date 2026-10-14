//! Secret-shape heuristic shared by the fail-closed refusal surfaces.
//!
//! Spec `40-arbitraitor-integration.md` §9.23.4 heuristic classes: `secret://`
//! URIs, `sk-`-prefixed keys, Bearer tokens, GitHub token shapes, long
//! hex/base64 runs, credential-header shapes. The heuristic is deliberately
//! conservative: false refusals are recoverable (the model rephrases), a
//! secret that persisted into an audit trail is not. Classification ownership
//! stays with Arbitraitor; this local scan mirrors the documented shapes so
//! string surfaces can refuse fail-closed without a policy round-trip.

/// Returns whether `value` carries secret-shaped material per the §9.23.4
/// heuristic classes. Never inspects anything but the input string; the
/// matching content never enters any error message produced by callers.
#[must_use]
pub fn contains_secret_shaped(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    // secret:// URIs (§9.23 secret referencing).
    if lower.contains("secret://") {
        return true;
    }
    // GitHub token shapes.
    if ["ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_"]
        .iter()
        .any(|prefix| lower.contains(prefix))
    {
        return true;
    }
    // `sk-`-prefixed keys (§9.23.4) — a real key continues with a long run
    // of token characters; prose containing the bare prefix stays allowed.
    // EVERY occurrence is inspected: an early short "sk-" in prose must not
    // mask a later real key.
    for (index, _) in lower.match_indices("sk-") {
        let token_chars: usize = lower[index + 3..]
            .chars()
            .take_while(|character| {
                character.is_ascii_alphanumeric() || *character == '_' || *character == '-'
            })
            .count();
        if token_chars >= 16 {
            return true;
        }
    }
    // Bearer tokens and credential headers (§20.4.2, §9.23.4 field names).
    if lower.contains("bearer ") && long_token_after(&lower, "bearer ") {
        return true;
    }
    if ["authorization:", "x-api-key:", "x-goog-api-key:"]
        .iter()
        .any(|marker| long_token_after(&lower, marker))
    {
        return true;
    }
    // Long hex runs: a 32+ hex run is secret-shaped (digest-length keys)
    // until proven otherwise.
    if hex_runs(value).iter().any(|run| *run >= 32) {
        return true;
    }
    // Long base64/base64url runs (§9.23.4 "long base64"): the shape of an
    // encoded key or JWT signature segment. Pure prose (no digits) is
    // never flagged.
    is_long_base64_run(value)
}

/// Whether any occurrence of `marker` is followed by a credential-shaped
/// token run of at least 16 characters. For credential headers the token
/// may be preceded by one authentication-scheme word (`Basic`, `Bearer`,
/// `token`, …), so one leading whitespace-delimited word is skipped before
/// measuring — `Authorization: Basic dXNlcjpwYXNzd29yZA==` measures the
/// base64 credential, not the scheme.
fn long_token_after(lower: &str, marker: &str) -> bool {
    lower.match_indices(marker).any(|(index, matched)| {
        let rest = &lower[index + matched.len()..];
        let mut words = rest
            .split(|character: char| {
                character.is_whitespace() || character == '"' || character == '\''
            })
            .filter(|word| !word.is_empty());
        // One optional leading auth-scheme word (e.g. `basic`), then the
        // credential token itself.
        let credential = match words.next() {
            Some(first) if is_auth_scheme_word(first) => words.next(),
            other => other,
        };
        credential.is_some_and(|word| word.chars().count() >= 16)
    })
}

/// Whether a whitespace-delimited word is an authentication-scheme label
/// that may precede the credential token in a header value.
fn is_auth_scheme_word(word: &str) -> bool {
    matches!(
        word,
        "basic" | "bearer" | "token" | "digest" | "negotiate" | "ntlm" | "oauth"
    )
}

/// Returns the lengths of maximal runs of ASCII hex characters in `value`.
fn hex_runs(value: &str) -> Vec<usize> {
    let mut runs = Vec::new();
    let mut current = 0usize;
    for character in value.chars() {
        if character.is_ascii_hexdigit() {
            current += 1;
        } else {
            if current > 0 {
                runs.push(current);
            }
            current = 0;
        }
    }
    if current > 0 {
        runs.push(current);
    }
    runs
}

/// Detects a ≥43-character run of the base64/base64url alphabet containing
/// at least one letter and one digit (the shape of an encoded key or JWT
/// signature segment, not of prose or a URL slug).
fn is_long_base64_run(value: &str) -> bool {
    let mut run_chars = 0usize;
    let mut has_letter = false;
    let mut has_digit = false;
    for character in value.chars() {
        let in_alphabet =
            character.is_ascii_alphanumeric() || matches!(character, '+' | '/' | '=' | '-' | '_');
        if in_alphabet {
            run_chars += 1;
            has_letter |= character.is_ascii_alphabetic();
            has_digit |= character.is_ascii_digit();
        } else {
            run_chars = 0;
            has_letter = false;
            has_digit = false;
        }
        if run_chars >= 43 && has_letter && has_digit {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::contains_secret_shaped;

    #[test]
    fn secret_uri_and_github_shapes_are_refused() {
        assert!(contains_secret_shaped(
            "resolve secret://env/NEURALWATT_API_KEY first"
        ));
        assert!(contains_secret_shaped(
            "key is ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdef"
        ));
        assert!(contains_secret_shaped(
            "pat: github_pat_ABCDEFGHIJKLMNOPQRSTUVWXYZ12"
        ));
        assert!(contains_secret_shaped(
            "token gho_ABCDEFGHIJKLMNOPQRSTUVWXYZ12"
        ));
    }

    #[test]
    fn sk_prefixed_keys_and_bearer_tokens_are_refused() {
        assert!(contains_secret_shaped(
            "using sk-proj-abcdefghijklmnop1234 for the call"
        ));
        assert!(contains_secret_shaped(
            "Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.payload.signature"
        ));
        assert!(contains_secret_shaped(
            "x-api-key: abcdefghijklmnopqrstuvwxyz123456"
        ));
    }

    #[test]
    fn long_hex_and_base64_runs_are_refused() {
        assert!(contains_secret_shaped(
            "digest a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6 confirmed"
        ));
        assert!(contains_secret_shaped(
            "signature dGhpc2lzYWJhc2U2NHNpZ25hdHVyZXZhbHVlMTIzNDU2Nzg5MA=="
        ));
    }

    #[test]
    fn prose_is_not_refused() {
        assert!(!contains_secret_shaped(
            "we should read the crate and check the sk- prefix handling"
        ));
        assert!(!contains_secret_shaped(
            "the plan is to refactor the parser in three steps"
        ));
        assert!(!contains_secret_shaped(
            "step 2 of 7: run clippy and fix warnings"
        ));
        assert!(!contains_secret_shaped("see issue #597 for the spec home"));
    }
}
