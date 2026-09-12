//! Model pricing + cost estimation. Providers don't return cost,
//! so we estimate it from token counts and a per-model price table (USD per
//! million tokens). Unknown models yield `None` (cost shown as "-").

/// Price per 1M tokens (USD).
#[derive(Debug, Clone, Copy)]
pub struct Pricing {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

/// Estimate cost from token counts. Returns `None` when the model has no entry.
pub fn compute_cost(
    model: &str,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
) -> Option<f64> {
    let p = pricing_for(model)?;
    let m = 1_000_000.0;
    Some(
        (input as f64 * p.input
            + output as f64 * p.output
            + cache_read as f64 * p.cache_read
            + cache_write as f64 * p.cache_write)
            / m,
    )
}

/// Look up pricing by (provider-stripped) model name; `None` = unknown.
pub fn pricing_for(model: &str) -> Option<Pricing> {
    crate::ModelCatalog::global()
        .pricing(model)
        .map(|p| Pricing {
            input: p.input,
            output: p.output,
            cache_read: p.cache_read,
            cache_write: p.cache_write,
        })
}
