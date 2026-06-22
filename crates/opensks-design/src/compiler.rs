//! Design compiler: token alias/type/cycle resolution, deterministic platform
//! adapters (Swift + CSS), a prompt relevance/budget adapter, and design-context
//! pinning (PR-038).
//!
//! The platform-neutral token IR ([`DesignTokenSet`]) may carry *alias* values:
//! a token whose `value` is a `{other.token.path}` reference instead of a
//! concrete scalar. Before a token set is compiled into any adapter it is first
//! **resolved**: every alias is followed transitively to a concrete value, with
//! three classes of error reported up front and never silently dropped:
//!
//! * a **cycle** (`a -> b -> a`) — [`DesignCompileError::AliasCycle`],
//! * an **unresolved** alias (references a missing token) —
//!   [`DesignCompileError::UnresolvedAlias`], and
//! * a **type mismatch** (e.g. a `color` token aliasing a `dimension`) —
//!   [`DesignCompileError::AliasTypeMismatch`].
//!
//! Resolution preserves the *input token order* (it only rewrites alias values
//! to their concrete targets, never reorders), so the Swift adapter stays
//! byte-identical for an alias-free set. The CSS adapter sorts its custom
//! properties by token path, and both adapters use only ordered iteration — no
//! `HashMap`, no timestamps, no randomness — so compiling the same input twice
//! yields byte-identical output.

use std::collections::BTreeMap;

use opensks_contracts::{
    DESIGN_CONTEXT_PACK_SCHEMA, DESIGN_CONTEXT_PIN_SCHEMA, DesignContextItem,
    DesignContextItemKind, DesignContextPack, DesignContextPin, DesignPackageComponent,
    DesignPackageComponents,
};

use crate::contracts::{DesignToken, DesignTokenSet};

/// Errors surfaced when resolving a token set's aliases. Content-free beyond the
/// token paths and stable reason codes needed to locate the offending token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesignCompileError {
    /// A token's alias chain loops back on itself (e.g. `a -> b -> a`). `path`
    /// is the token where resolution started; `cycle` is the chain of paths
    /// visited, in order, ending at the repeated path.
    AliasCycle { path: String, cycle: Vec<String> },
    /// A token aliases a path that does not exist in the set.
    UnresolvedAlias { path: String, missing: String },
    /// A token's declared `type` differs from the type of the token it resolves
    /// to (e.g. a `color` aliasing a `dimension`).
    AliasTypeMismatch {
        path: String,
        expected_type: String,
        target: String,
        actual_type: String,
    },
    /// Two tokens declare the same path; resolution cannot pick a winner.
    DuplicateTokenPath { path: String },
}

impl DesignCompileError {
    /// Stable machine-readable reason code.
    pub fn reason_code(&self) -> &'static str {
        match self {
            Self::AliasCycle { .. } => "design_alias_cycle",
            Self::UnresolvedAlias { .. } => "design_alias_unresolved",
            Self::AliasTypeMismatch { .. } => "design_alias_type_mismatch",
            Self::DuplicateTokenPath { .. } => "design_token_duplicate_path",
        }
    }
}

impl std::fmt::Display for DesignCompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AliasCycle { path, cycle } => write!(
                f,
                "{}: token {path} alias cycle {}",
                self.reason_code(),
                cycle.join(" -> ")
            ),
            Self::UnresolvedAlias { path, missing } => write!(
                f,
                "{}: token {path} aliases missing token {missing}",
                self.reason_code()
            ),
            Self::AliasTypeMismatch {
                path,
                expected_type,
                target,
                actual_type,
            } => write!(
                f,
                "{}: token {path} ({expected_type}) aliases {target} ({actual_type})",
                self.reason_code()
            ),
            Self::DuplicateTokenPath { path } => {
                write!(
                    f,
                    "{}: token path {path} declared twice",
                    self.reason_code()
                )
            }
        }
    }
}

impl std::error::Error for DesignCompileError {}

/// If `value` is an alias string of the form `{token.path}`, return the
/// referenced path. A concrete scalar (hex string, number) is not an alias.
fn alias_target(value: &serde_json::Value) -> Option<&str> {
    let text = value.as_str()?;
    let inner = text.strip_prefix('{')?.strip_suffix('}')?;
    if inner.is_empty() {
        return None;
    }
    Some(inner)
}

/// A token set whose every token holds a concrete (non-alias) value, with alias
/// chains followed, cycles/unresolved/type errors already rejected. Token order
/// is preserved from the input set, so adapters stay byte-stable.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedTokenSet {
    pub design_system_id: String,
    pub revision: u32,
    pub tokens: Vec<DesignToken>,
}

impl ResolvedTokenSet {
    /// Look up a resolved token by path.
    pub fn get(&self, path: &str) -> Option<&DesignToken> {
        self.tokens.iter().find(|t| t.path == path)
    }
}

/// Resolve every alias in `set` to a concrete value, preserving input order.
///
/// Algorithm: index tokens by path (rejecting duplicate paths). For each token,
/// walk its alias chain — tracking the ordered set of visited paths — until a
/// concrete value is reached. A repeat visit is a cycle; a missing path is
/// unresolved; a type change along the chain is a type mismatch. The resolved
/// token keeps its own `path`/`type`/metadata but adopts the concrete `value`.
pub fn resolve_token_set(set: &DesignTokenSet) -> Result<ResolvedTokenSet, DesignCompileError> {
    // Index by path; duplicate paths are ambiguous and rejected.
    let mut index: BTreeMap<&str, &DesignToken> = BTreeMap::new();
    for token in &set.tokens {
        if index.insert(token.path.as_str(), token).is_some() {
            return Err(DesignCompileError::DuplicateTokenPath {
                path: token.path.clone(),
            });
        }
    }

    let mut resolved = Vec::with_capacity(set.tokens.len());
    for token in &set.tokens {
        let (value, _) = resolve_alias_chain(token, &index)?;
        let mut concrete = token.clone();
        concrete.value = value;
        resolved.push(concrete);
    }

    Ok(ResolvedTokenSet {
        design_system_id: set.design_system_id.clone(),
        revision: set.revision,
        tokens: resolved,
    })
}

/// Follow `token`'s alias chain to a concrete value, enforcing cycle, missing,
/// and type-mismatch invariants. Returns the concrete value plus the path it
/// ultimately resolved to.
fn resolve_alias_chain(
    token: &DesignToken,
    index: &BTreeMap<&str, &DesignToken>,
) -> Result<(serde_json::Value, String), DesignCompileError> {
    let start = &token.path;
    let expected_type = &token.token_type;
    let mut visited: Vec<String> = vec![start.clone()];
    let mut current = token;

    loop {
        let Some(target_path) = alias_target(&current.value) else {
            // Concrete value reached.
            return Ok((current.value.clone(), current.path.clone()));
        };

        // Cycle: the alias points back at a path already on the chain.
        if visited.iter().any(|p| p == target_path) {
            let mut cycle = visited.clone();
            cycle.push(target_path.to_string());
            return Err(DesignCompileError::AliasCycle {
                path: start.clone(),
                cycle,
            });
        }

        // Unresolved: the alias points at a path not in the set.
        let Some(next) = index.get(target_path) else {
            return Err(DesignCompileError::UnresolvedAlias {
                path: start.clone(),
                missing: target_path.to_string(),
            });
        };

        // Type mismatch: an alias must point at a token of the same declared
        // type (a color may not resolve to a dimension/number, etc.).
        if next.token_type != *expected_type {
            return Err(DesignCompileError::AliasTypeMismatch {
                path: start.clone(),
                expected_type: expected_type.clone(),
                target: next.path.clone(),
                actual_type: next.token_type.clone(),
            });
        }

        visited.push(target_path.to_string());
        current = next;
    }
}

/// Compile a resolved token set into deterministic CSS custom properties.
///
/// Emits a single `:root { ... }` block with one `--<dashed-path>: <value>;`
/// declaration per token, **sorted by token path** so output is byte-identical
/// across runs regardless of input order. Colors render as their hex string,
/// dimensions as `<number><unit>` (defaulting to `px` when no unit is declared),
/// other scalar values verbatim.
pub fn compile_css_tokens(set: &ResolvedTokenSet) -> String {
    let mut sorted: Vec<&DesignToken> = set.tokens.iter().collect();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));

    let mut out = String::new();
    out.push_str("/* GeneratedDesignTokens.css */\n");
    out.push_str(&format!(
        "/* Source of truth: .opensks/design-systems/{}/tokens.json */\n",
        set.design_system_id
    ));
    out.push_str("/* Do not edit by hand. Regenerate via the opensks-design compiler. */\n");
    out.push_str(":root {\n");
    out.push_str(&format!("  --revision: {};\n", set.revision));
    for token in sorted {
        let name = css_property_name(&token.path);
        let value = css_value(token);
        out.push_str(&format!("  {name}: {value};\n"));
    }
    out.push_str("}\n");
    out
}

/// Map a token path (`color.text.primary`) to a CSS custom-property name
/// (`--color-text-primary`): lowercase, `.`/`_` to `-`.
fn css_property_name(path: &str) -> String {
    let mut name = String::from("--");
    for ch in path.chars() {
        if ch == '.' || ch == '_' {
            name.push('-');
        } else {
            name.extend(ch.to_lowercase());
        }
    }
    name
}

/// Render a resolved token's concrete value for CSS.
fn css_value(token: &DesignToken) -> String {
    if token.token_type == "color" {
        if let Some(text) = token.value.as_str() {
            // Hex colors keep their leading `#`.
            if let Some(stripped) = text.strip_prefix('#') {
                return format!("#{stripped}");
            }
            return text.to_string();
        }
    }
    if token.token_type == "dimension" {
        let number = format_number(&token.value);
        let unit = match token.unit.as_deref() {
            Some("pt") | None => "px",
            Some(other) => other,
        };
        return format!("{number}{unit}");
    }
    match &token.value {
        serde_json::Value::String(s) => s.clone(),
        other => format_number(other),
    }
}

/// Format a JSON number deterministically: integers without a fractional part.
fn format_number(value: &serde_json::Value) -> String {
    if let Some(n) = value.as_i64() {
        return n.to_string();
    }
    if let Some(n) = value.as_u64() {
        return n.to_string();
    }
    if let Some(n) = value.as_f64() {
        if n.fract() == 0.0 {
            return (n as i64).to_string();
        }
        return n.to_string();
    }
    "0".to_string()
}

/// A design context the prompt adapter selects from: the resolved tokens plus an
/// optional component catalog, bound to one design system.
#[derive(Debug, Clone)]
pub struct DesignContext<'a> {
    pub tokens: &'a ResolvedTokenSet,
    pub components: Option<&'a DesignPackageComponents>,
}

impl<'a> DesignContext<'a> {
    pub fn new(tokens: &'a ResolvedTokenSet) -> Self {
        Self {
            tokens,
            components: None,
        }
    }

    pub fn with_components(mut self, components: &'a DesignPackageComponents) -> Self {
        self.components = Some(components);
        self
    }
}

/// A candidate datum scored for relevance during pack selection.
struct Candidate {
    item: DesignContextItem,
    /// Higher is more relevant.
    score: u32,
}

/// Render a token as a single deterministic context line.
fn token_line(token: &DesignToken) -> String {
    let value = match &token.value {
        serde_json::Value::String(s) => s.clone(),
        other => format_number(other),
    };
    format!("token {} ({}) = {value}", token.path, token.token_type)
}

/// Render a component as a single deterministic context line.
fn component_line(component: &DesignPackageComponent) -> String {
    let refs = component.token_refs.join(",");
    format!("component {} [{}]", component.id, refs)
}

/// Score one searchable string against a query of lowercased terms. A term that
/// appears scores; an exact full-string match scores higher; terms are summed.
fn relevance_score(haystack: &str, terms: &[&str]) -> u32 {
    let lower = haystack.to_lowercase();
    let mut score = 0;
    for term in terms {
        if lower == *term {
            score += 3;
        } else if lower.contains(term) {
            score += 1;
        }
    }
    score
}

/// Build a budget-bounded, relevance-selected [`DesignContextPack`].
///
/// Selection is deterministic: every token and component is scored against the
/// lowercased query terms (matched against its reference, type/role, and rendered
/// text). Items with a zero score are excluded as irrelevant. Survivors are
/// ordered by `(-score, reference)` — most relevant first, ties broken by
/// reference — then admitted greedily while they fit BOTH the item budget
/// (`max_items`) and the character budget (`max_chars`). The same
/// `(context, query, max_items, max_chars)` always yields a byte-identical pack.
pub fn build_design_context_pack(
    context: &DesignContext<'_>,
    query: &str,
    max_items: u32,
    max_chars: u32,
) -> DesignContextPack {
    let lower_query = query.to_lowercase();
    let terms: Vec<&str> = lower_query
        .split_whitespace()
        .filter(|t| !t.is_empty())
        .collect();

    let mut candidates: Vec<Candidate> = Vec::new();

    for token in &context.tokens.tokens {
        let text = token_line(token);
        // Score the path, the type, the semantic role, and the rendered line.
        let mut score = relevance_score(&token.path, &terms)
            + relevance_score(&token.token_type, &terms)
            + relevance_score(&text, &terms);
        if let Some(role) = &token.semantic_role {
            score += relevance_score(role, &terms);
        }
        if score == 0 {
            continue;
        }
        let char_cost = u32::try_from(text.len() + 1).unwrap_or(u32::MAX);
        candidates.push(Candidate {
            item: DesignContextItem {
                kind: DesignContextItemKind::Token,
                reference: token.path.clone(),
                text,
                char_cost,
            },
            score,
        });
    }

    if let Some(components) = context.components {
        for component in &components.components {
            let text = component_line(component);
            let mut score = relevance_score(&component.id, &terms)
                + relevance_score(&component.name, &terms)
                + relevance_score(&text, &terms);
            if let Some(description) = &component.description {
                score += relevance_score(description, &terms);
            }
            if score == 0 {
                continue;
            }
            let char_cost = u32::try_from(text.len() + 1).unwrap_or(u32::MAX);
            candidates.push(Candidate {
                item: DesignContextItem {
                    kind: DesignContextItemKind::Component,
                    reference: component.id.clone(),
                    text,
                    char_cost,
                },
                score,
            });
        }
    }

    // Deterministic ordering: most relevant first, ties by reference.
    candidates.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.item.reference.cmp(&b.item.reference))
    });

    let mut items: Vec<DesignContextItem> = Vec::new();
    let mut total_chars: u32 = 0;
    for candidate in candidates {
        if items.len() as u32 >= max_items {
            break;
        }
        let next_total = total_chars.saturating_add(candidate.item.char_cost);
        if next_total > max_chars {
            // Over budget: skip this item but keep scanning — a smaller, equally
            // ranked item later in the order may still fit.
            continue;
        }
        total_chars = next_total;
        items.push(candidate.item);
    }

    let body = items
        .iter()
        .map(|item| item.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    DesignContextPack {
        schema: DESIGN_CONTEXT_PACK_SCHEMA.to_string(),
        design_system_id: context.tokens.design_system_id.clone(),
        query: lower_query,
        max_items,
        max_chars,
        items,
        total_chars,
        body,
    }
}

/// Stable FNV-1a content hash matching the repo's `fnv1a64:` convention.
fn fnv1a64(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("fnv1a64:{hash:016x}")
}

/// Pin a context pack to a `(model id, graph/revision)` identity.
///
/// The pin's `pack_hash` is a stable digest over the identity (model id, graph
/// revision, design system) and the pack body, so a pinned context can be
/// referenced later and re-verified by recomputing the hash from the same
/// inputs. Deterministic: identical inputs yield an identical pin.
pub fn pin_design_context(
    pack: &DesignContextPack,
    model_id: &str,
    graph_revision: &str,
) -> DesignContextPin {
    // A field-separated, order-stable preimage. `\u{1f}` (unit separator) keeps
    // fields unambiguous without colliding with body text.
    let mut preimage = String::new();
    preimage.push_str(model_id);
    preimage.push('\u{1f}');
    preimage.push_str(graph_revision);
    preimage.push('\u{1f}');
    preimage.push_str(&pack.design_system_id);
    preimage.push('\u{1f}');
    preimage.push_str(&pack.query);
    preimage.push('\u{1f}');
    preimage.push_str(&pack.body);

    DesignContextPin {
        schema: DESIGN_CONTEXT_PIN_SCHEMA.to_string(),
        model_id: model_id.to_string(),
        graph_revision: graph_revision.to_string(),
        design_system_id: pack.design_system_id.clone(),
        pack_hash: fnv1a64(preimage.as_bytes()),
    }
}

/// Verify a pin matches a `(pack, model id, graph/revision)` identity by
/// recomputing the hash. Returns `true` when the pin is still valid.
pub fn verify_design_context_pin(
    pin: &DesignContextPin,
    pack: &DesignContextPack,
    model_id: &str,
    graph_revision: &str,
) -> bool {
    let recomputed = pin_design_context(pack, model_id, graph_revision);
    recomputed.pack_hash == pin.pack_hash
        && pin.model_id == model_id
        && pin.graph_revision == graph_revision
        && pin.design_system_id == pack.design_system_id
}
