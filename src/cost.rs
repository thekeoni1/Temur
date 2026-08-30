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

/// The estimate inputs of a selection that CAN be estimated: the provider
/// (which decides the cache terms) plus the pair of configured list rates.
///
/// Constructing one is the SELECTION half of the estimate gate, so holding a
/// `CostRates` is proof that a keyed, priced selection is active. The session
/// carries one (T26) instead of a `ResolvedProfile` it has no other use for,
/// and both consumers, `/status` and the mid-session advisory, reach the
/// estimate through this same type: the gate exists once.
#[derive(Debug, Clone, PartialEq)]
pub struct CostRates {
    pub provider: String,
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
}

impl CostRates {
    /// The rates for `active`, or `None` when no estimate may be shown for
    /// it: keyless (nobody is billed for a local server) or unpriced (an
    /// unpriced profile gets no nag; the docs point at the fields).
    pub fn for_profile(active: &ResolvedProfile) -> Option<CostRates> {
        match (active.price_input_per_mtok, active.price_output_per_mtok) {
            (Some(i), Some(o)) if active.is_keyed() => Some(CostRates {
                provider: active.provider.clone(),
                input_per_mtok: i,
                output_per_mtok: o,
            }),
            _ => None,
        }
    }

    /// The estimate for `usage`, or `None` when the session has reported no
    /// usage at all (nothing to estimate before the first response). The
    /// USAGE half of the gate.
    pub fn estimate(&self, usage: &Usage) -> Option<f64> {
        reported_anything(usage)
            .then(|| estimate_usd(&self.provider, usage, self.input_per_mtok, self.output_per_mtok))
    }
}

/// The `/status` estimate, or `None` when the line must be ABSENT.
///
/// Three conditions, all required: the active selection is keyed, both list
/// prices are configured, and the session has some reported usage. Any one
/// missing means silence, not a placeholder. Both halves come from
/// [`CostRates`], so the mid-session advisory (T26) cannot drift from the
/// line `/status` shows.
pub fn session_estimate_usd(active: &ResolvedProfile, usage: &Usage) -> Option<f64> {
    CostRates::for_profile(active)?.estimate(usage)
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

// ------------------------------------------------- T26 mid-session advisory

/// How many whole steps an estimate covers: the step multiple `floor(estimate
/// / step)`. This is BOTH the latch's initial value and the value a crossing
/// moves it to, so "already accounted for" means the same arithmetic
/// everywhere.
///
/// Zero for a disabled (`0`) step, and for anything non-finite or negative
/// that reached here despite config validation: a number nobody can act on
/// must never produce an advisory.
pub fn step_multiple(estimate_usd: f64, step_usd: f64) -> u64 {
    if !step_usd.is_finite() || step_usd <= 0.0 || !estimate_usd.is_finite() || estimate_usd < 0.0 {
        return 0;
    }
    // Saturating `as` cast: a step multiple past u64 is unreachable spend,
    // and clamping there is still monotonic, so the latch cannot go backward.
    (estimate_usd / step_usd).floor() as u64
}

/// The step multiple to advise at, or `None` for silence.
///
/// `latch` is the highest multiple already accounted for. A jump that clears
/// several multiples at once returns only the HIGHEST: one advisory naming
/// where the session actually is, never a burst of one line per step it flew
/// past. The caller stores the returned value as the new latch.
///
/// Pure by design (no session, no config, no clock): the whole trigger
/// decision is these three numbers, and it is unit-tested as such.
pub fn advisory_crossing(estimate_usd: f64, step_usd: f64, latch: u64) -> Option<u64> {
    let reached = step_multiple(estimate_usd, step_usd);
    (reached > latch).then_some(reached)
}

/// The advisory line for a crossing of `multiple` steps of `step_usd`, at a
/// current estimate of `estimate_usd`.
///
/// Says "estimate" (this is an awareness figure computed from list rates, not
/// a bill), names the threshold crossed AND where the session now stands (so
/// a jump well past the threshold reads as the jump it was), and names the
/// field that tunes or silences it. One line, no em-dashes.
pub fn advisory_message(multiple: u64, step_usd: f64, estimate_usd: f64) -> String {
    let crossed = multiple as f64 * step_usd;
    format!(
        "cost: this session has crossed ${} (estimate: ~${} at configured list rates); set cost_advisory_step_usd to change the step or 0 to disable",
        format_usd(crossed),
        format_usd(estimate_usd)
    )
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
            prompt_profile_source: Default::default(),
            price_input_per_mtok: prices.map(|(i, _)| i),
            price_output_per_mtok: prices.map(|(_, o)| o),
            max_tokens_parameter: Default::default(),
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
    fn no_crossing_below_the_next_multiple() {
        // Latched at $5, sitting at $9.99: the next multiple is not reached.
        assert_eq!(advisory_crossing(9.99, 5.0, 1), None);
        // And nothing at all before the first step.
        assert_eq!(advisory_crossing(4.99, 5.0, 0), None);
    }

    #[test]
    fn the_exact_boundary_crosses() {
        // $5.00 exactly IS the first multiple, not one cent short of it.
        assert_eq!(advisory_crossing(5.0, 5.0, 0), Some(1));
        assert_eq!(advisory_crossing(10.0, 5.0, 1), Some(2));
    }

    #[test]
    fn a_multi_step_jump_advises_once_at_the_highest() {
        // The $26 turn: one response takes the session from under $5 to $26.
        // ONE advisory, naming $25, not five lines.
        assert_eq!(advisory_crossing(26.0, 5.0, 0), Some(5));
        // And the new latch suppresses everything up to the next multiple.
        assert_eq!(advisory_crossing(29.99, 5.0, 5), None);
        assert_eq!(advisory_crossing(30.0, 5.0, 5), Some(6));
    }

    #[test]
    fn step_zero_never_fires() {
        // 0 is the documented disable, at any spend and any latch.
        assert_eq!(advisory_crossing(1_000.0, 0.0, 0), None);
        assert_eq!(step_multiple(1_000.0, 0.0), 0);
        // Values config validation rejects can never advise either.
        assert_eq!(advisory_crossing(1_000.0, -5.0, 0), None);
        assert_eq!(advisory_crossing(f64::NAN, 5.0, 0), None);
        assert_eq!(advisory_crossing(f64::INFINITY, 5.0, 0), None);
    }

    #[test]
    fn the_latch_initializes_to_the_floor_of_current_spend() {
        // Resuming a session that already spent $26 starts latched at 5, so
        // the money already spent cannot fire.
        assert_eq!(step_multiple(26.0, 5.0), 5);
        assert_eq!(advisory_crossing(26.0, 5.0, step_multiple(26.0, 5.0)), None);
        // Fresh session, nothing spent.
        assert_eq!(step_multiple(0.0, 5.0), 0);
        // Fractional steps floor the same way.
        assert_eq!(step_multiple(1.0, 0.25), 4);
    }

    #[test]
    fn the_advisory_line_names_the_threshold_the_estimate_and_the_field() {
        let line = advisory_message(5, 5.0, 26.4213);
        assert_eq!(
            line,
            "cost: this session has crossed $25.00 (estimate: ~$26.42 at configured list rates); set cost_advisory_step_usd to change the step or 0 to disable"
        );
        assert!(!line.contains('\n'), "one line only: {line}");
        assert!(!line.contains('\u{2014}'), "no em-dashes: {line}");
    }

    #[test]
    fn formatting_shows_four_decimals_below_a_cent() {
        assert_eq!(format_usd(1.239), "1.24");
        assert_eq!(format_usd(0.01), "0.01");
        assert_eq!(format_usd(0.0042), "0.0042");
        assert_eq!(format_usd(0.0), "0.0000");
    }
}
