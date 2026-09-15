/// Converts an arbitrary display name into a URL-safe slug.
///
/// Non-alphanumeric characters become hyphens, repeated hyphens are
/// collapsed, and leading/trailing hyphens are trimmed. Callers are
/// responsible for ensuring uniqueness (see `uniquify_slug`).
pub fn slugify(name: &str) -> String {
    let mut slug = String::with_capacity(name.len());
    let mut last_was_hyphen = false;

    for c in name.trim().chars() {
        if c.is_alphanumeric() {
            slug.extend(c.to_lowercase());
            last_was_hyphen = false;
        } else if !last_was_hyphen && !slug.is_empty() {
            slug.push('-');
            last_was_hyphen = true;
        }
    }

    while slug.ends_with('-') {
        slug.pop();
    }

    if slug.is_empty() {
        slug.push_str("band");
    }

    slug.chars().take(70).collect()
}

/// Appends a short random suffix to a slug, used to resolve collisions
/// without a round-trip back to the user.
pub fn uniquify_slug(base_slug: &str) -> String {
    format!(
        "{base_slug}-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..6]
    )
}
