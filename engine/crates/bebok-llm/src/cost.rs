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
    let name = model.rsplit('/').next().unwrap_or(model).to_ascii_lowercase();
    let p = match name.as_str() {
        // Z.ai GLM
        "glm-5.3-flash" => p(0.60, 2.20, 0.10, 0.60),
        "glm-4.7-flash" => p(0.60, 2.20, 0.10, 0.60),
        "glm-4.6" => p(0.60, 2.20, 0.10, 0.60),
        "glm-4.5-air" => p(0.14, 0.60, 0.04, 0.14),

        // OpenAI
        "gpt-4.1" => p(2.0, 8.0, 0.50, 2.0),
        "gpt-4.1-mini" => p(0.40, 1.60, 0.10, 0.40),
        "gpt-4o" => p(2.50, 10.0, 0.63, 2.50),
        "gpt-4o-mini" => p(0.15, 0.60, 0.038, 0.15),
        "o3-mini" => p(1.10, 4.40, 0.28, 1.10),

        // Anthropic
        "claude-opus-4-5" => p(15.0, 75.0, 1.50, 18.75),
        "claude-sonnet-4-5" => p(3.0, 15.0, 0.30, 3.75),
        "claude-haiku-4-5" => p(1.0, 5.0, 0.10, 1.25),

        // DeepSeek
        "deepseek-chat" => p(0.27, 1.10, 0.07, 0.27),
        "deepseek-reasoner" => p(0.55, 2.19, 0.14, 0.55),

        // Google
        "gemini-2.5-pro" => p(1.25, 10.0, 0.31, 1.25),
        "gemini-2.5-flash" => p(0.30, 2.50, 0.075, 0.30),

        // xAI
        "grok-4.6" => p(2.0, 8.0, 0.0, 0.0),
        "grok-4.3" => p(2.0, 8.0, 0.0, 0.0),

        // Qwen
        "qwen-plus" => p(0.40, 1.20, 0.0, 0.0),
        "qwen3-235b-a22b" => p(0.40, 1.20, 0.0, 0.0),

        // Mistral
        "mistral-large-latest" => p(2.0, 6.0, 0.0, 0.0),
        "mistral-medium-latest" => p(0.80, 2.40, 0.0, 0.0),

        // Groq (Llama)
        "llama-3.3-70b-versatile" => p(0.59, 0.79, 0.0, 0.0),

        _ => return None,
    };
    Some(p)
}

const fn p(input: f64, output: f64, cache_read: f64, cache_write: f64) -> Pricing {
    Pricing {
        input,
        output,
        cache_read,
        cache_write,
    }
}
