//! Session cost estimation (T24).
//!
//! An AWARENESS figure, never a bill: temur multiplies the token counts
//! the provider already reported for this session by the list prices the
//! operator configured, entirely offline. Nothing here talks to a billing
//! API, and no provider exposes one to talk to. Every consumer wording
//! must say "estimate" for that reason.
//!
//! Absent (`None`) usage fields contribute zero: a provider that does not
//! report cache tokens is indistinguishable from one that reports none,
//! and guessing in either direction would be worse than counting nothing.

use crate::config::ResolvedProfile;
use crate::provider::Usage;

/// Anthropic list multiplier on the INPUT rate for tokens served from the
/// prompt cache. Knowledge-of-record 2026-08-07 (Anthropic published
/// cache pricing: cache reads ~0.1x base input). Not auto-detected, and
/// not applied to any other provider.
const ANTHROPIC_CACHE_READ_MULTIPLIER: f64 = 0.1;

/// Anthropic list multiplier on the INPUT rate for tokens WRITTEN to the
/// prompt cache at the default 5-minute TTL. Knowledge-of-record
/// 2026-08-07 (Anthropic published cache pricing: cache writes 1.25x base
/// input for 5m, 2x for 1h; temur never requests the 1h TTL).
const ANTHROPIC_CACHE_WRITE_MULTIPLIER: f64 = 1.25;

/// Divisor turning per-million-token rates into per-token rates.
const TOKENS_PER_MTOK: f64 = 1_000_000.0;

/// The dollar estimate for one usage total at one pair of list rates.
///
/// Plain input and output tokens are billed at their own rates on every
/// provider. The Anthropic cache terms are added for the anthropic
/// provider ONLY, because that is the only wire where cache tokens are
/// reported as counts SEPARATE from `input_tokens`. On the openai-compat
/// wire, `cached_tokens` is a subset of `prompt_tokens` the caller has
/// already been charged for, so adding a cache term there would double
/// count; plain in/out slightly OVERSTATES compat spend instead, which is
/// the safe direction for a spend-awareness number.
///
/// f64 throughout on purpose: these are money-shaped magnitudes, not
/// sizes, so the 32-bit `usize` rule does not apply and no count here can
/// approach f64's exact-integer range.
pub fn estimate_usd(
    provider: &str,
    usage: &Usage,
    price_input_per_mtok: f64,
    price_output_per_mtok: f64,
) -> f64 {
    let tokens = |v: Option<u64>| v.unwrap_or(0) as f64;
    let mut total = tokens(usage.input_tokens) * price_input_per_mtok / TOKENS_PER_MTOK
        + tokens(usage.output_tokens) * price_output_per_mtok / TOKENS_PER_MTOK;
    if provider == "anthropic" {
        total += tokens(usage.cache_read_input_tokens) * price_input_per_mtok
            * ANTHROPIC_CACHE_READ_MULTIPLIER
            / TOKENS_PER_MTOK;
        total += tokens(usage.cache_creation_input_tokens) * price_input_per_mtok
            * ANTHROPIC_CACHE_WRITE_MULTIPLIER
            / TOKENS_PER_MTOK;
    }
    total
}

/// The `/status` estimate, or `None` when the line must be ABSENT.
///
/// Three conditions, all required: the active selection is keyed (nobody
/// is billed for a local server), both list prices are configured (an
/// unpriced profile gets no nag, the docs point at the fields), and the
/// session has some reported usage (nothing to estimate before the first
/// turn). Any one missing means silence, not a placeholder.
pub fn session_estimate_usd(active: &ResolvedProfile, usage: &Usage) -> Option<f64> {
    let (pin, pout) = match (active.price_input_per_mtok, active.price_output_per_mtok) {
        (Some(i), Some(o)) => (i, o),
        _ => return None,
    };
    if !active.is_keyed() || !reported_anything(usage) {
        return None;
    }
    Some(estimate_usd(&active.provider, usage, pin, pout))
}

/// Whether the session has any token count at all. `None` everywhere is
/// the pre-first-turn state (and the state a provider that reports no
/// usage stays in forever).
fn reported_anything(usage: &Usage) -> bool {
    usage.input_tokens.is_some()
        || usage.output_tokens.is_some()
        || usage.cache_creation_input_tokens.is_some()
        || usage.cache_read_input_tokens.is_some()
}

/// Render a dollar estimate: two decimals once there is a cent to show,
/// four below that so a real-but-small spend never renders as `$0.00`.
pub fn format_usd(value: f64) -> String {
    if value >= 0.01 {
        format!("{value:.2}")
    } else {
        format!("{value:.4}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::PromptProfile;

    fn usage(input: u64, output: u64, write: u64, read: u64) -> Usage {
        Usage {
            input_tokens: Some(input),
            output_tokens: Some(output),
            cache_creation_input_tokens: Some(write),
            cache_read_input_tokens: Some(read),
        }
    }

    fn profile(provider: &str, keyed: bool, prices: Option<(f64, f64)>) -> ResolvedProfile {
        ResolvedProfile {
            provider: provider.into(),
            model: "m".into(),
            base_url: "http://x/v1".into(),
            api_key_file: if keyed { Some("/tmp/k".into()) } else { None },
            max_tokens: 4096,
            context_window: None,
            prompt_profile: PromptProfile::Full,
            price_input_per_mtok: prices.map(|(i, _)| i),
            price_output_per_mtok: prices.map(|(_, o)| o),
        }
    }

    #[test]
    fn anthropic_estimate_includes_the_cache_terms() {
        // 1M in @3, 1M out @15, 1M cache-write @3*1.25, 1M cache-read @3*0.1.
        let got = estimate_usd("anthropic", &usage(1_000_000, 1_000_000, 1_000_000, 1_000_000), 3.0, 15.0);
        assert!((got - (3.0 + 15.0 + 3.75 + 0.3)).abs() < 1e-9, "{got}");
    }

    #[test]
    fn compat_estimate_omits_the_cache_terms() {
        // Same usage, same rates: only in/out count, because compat's
        // cached_tokens is a subset of prompt_tokens.
        let got = estimate_usd(
            "openai-compat",
            &usage(1_000_000, 1_000_000, 1_000_000, 1_000_000),
            3.0,
            15.0,
        );
        assert!((got - 18.0).abs() < 1e-9, "{got}");
    }

    #[test]
    fn absent_fields_contribute_zero() {
        let partial = Usage { output_tokens: Some(2_000_000), ..Usage::default() };
        let got = estimate_usd("anthropic", &partial, 3.0, 15.0);
        assert!((got - 30.0).abs() < 1e-9, "{got}");
    }

    #[test]
    fn zero_usage_is_zero_not_an_error() {
        assert_eq!(estimate_usd("anthropic", &usage(0, 0, 0, 0), 3.0, 15.0), 0.0);
    }

    #[test]
    fn the_status_gate_requires_keyed_priced_and_used() {
        let used = usage(1_000_000, 0, 0, 0);
        let unused = Usage::default();

        // Keyed compat + prices + usage: rendered.
        let p = profile("openai-compat", true, Some((3.0, 15.0)));
        assert_eq!(session_estimate_usd(&p, &used), Some(3.0));
        // Anthropic is keyed with no key file of its own.
        let p = profile("anthropic", false, Some((3.0, 15.0)));
        assert_eq!(session_estimate_usd(&p, &used), Some(3.0));
        // Keyless compat: never, even fully priced and used.
        let p = profile("openai-compat", false, Some((3.0, 15.0)));
        assert_eq!(session_estimate_usd(&p, &used), None);
        // Keyed but unpriced: no nag.
        let p = profile("openai-compat", true, None);
        assert_eq!(session_estimate_usd(&p, &used), None);
        // Priced but nothing reported yet: nothing to estimate.
        let p = profile("openai-compat", true, Some((3.0, 15.0)));
        assert_eq!(session_estimate_usd(&p, &unused), None);
    }

    #[test]
    fn formatting_shows_four_decimals_below_a_cent() {
        assert_eq!(format_usd(1.239), "1.24");
        assert_eq!(format_usd(0.01), "0.01");
        assert_eq!(format_usd(0.0042), "0.0042");
        assert_eq!(format_usd(0.0), "0.0000");
    }
}
