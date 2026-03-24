//! Local-first knowledge draft/card file management and prompt synthesis.

use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::config;
use crate::store::{KnowledgeCard, KnowledgeDraft, Store};

pub fn write_draft_markdown(draft: &KnowledgeDraft) -> Result<PathBuf> {
    let dir = knowledge_repo_bucket(
        draft.repo_owner.as_ref(),
        draft.repo_name.as_ref(),
        Kind::Drafts,
    )?;
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create draft knowledge dir {}", dir.display()))?;
    let path = dir.join(file_slug(&draft.title, &draft.id));
    fs::write(&path, render_draft(draft))
        .with_context(|| format!("failed to write draft knowledge card {}", path.display()))?;
    Ok(path)
}

pub fn write_card_markdown(card: &KnowledgeCard) -> Result<PathBuf> {
    let dir = knowledge_repo_bucket(
        card.repo_owner.as_ref(),
        card.repo_name.as_ref(),
        Kind::Cards,
    )?;
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create knowledge cards dir {}", dir.display()))?;
    let path = dir.join(file_slug(&card.title, &card.id));
    fs::write(&path, render_card(card))
        .with_context(|| format!("failed to write knowledge card {}", path.display()))?;
    Ok(path)
}

pub fn synthesize_context_markdown(store: &Store, project_id: &str) -> Result<String> {
    let cards = store.list_knowledge_cards_for_project(project_id)?;
    let mut output = String::from("# Claustre Context\n\n");
    for card in cards {
        let _ = writeln!(output, "## {}\n", card.title);
        let _ = writeln!(output, "{}\n", card.summary);
        output.push_str(&card.content_md);
        output.push_str("\n\n");
    }
    Ok(output)
}

pub fn persist_project_context_snapshot(
    store: &Store,
    project_id: &str,
    repo_owner: &str,
    repo_name: &str,
) -> Result<PathBuf> {
    let path = config::knowledge_artifacts_dir(repo_owner, repo_name)?.join("context.md");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create context dir {}", parent.display()))?;
    }
    fs::write(&path, synthesize_context_markdown(store, project_id)?)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

fn render_draft(draft: &KnowledgeDraft) -> String {
    format!(
        "---\nkind: knowledge_draft\nid: {}\nproject_id: {}\nconfidence: {}\ntags: {}\ncreated_at: {}\n---\n\n# {}\n\n{}\n\n{}",
        draft.id,
        draft.project_id,
        draft
            .confidence
            .map_or_else(|| "unknown".to_string(), |confidence| confidence.to_string()),
        serde_json::to_string(&draft.tags).unwrap_or_else(|_| "[]".to_string()),
        draft.created_at,
        draft.title,
        draft.summary,
        draft.content_md
    )
    .trim_end()
    .to_string()
}

fn render_card(card: &KnowledgeCard) -> String {
    format!(
        "---\nkind: knowledge_card\nid: {}\nproject_id: {}\nconfidence: {}\ntags: {}\naccepted_at: {}\n---\n\n# {}\n\n{}\n\n{}",
        card.id,
        card.project_id,
        card.confidence
            .map_or_else(|| "unknown".to_string(), |confidence| confidence.to_string()),
        serde_json::to_string(&card.tags).unwrap_or_else(|_| "[]".to_string()),
        card.accepted_at.as_deref().unwrap_or(""),
        card.title,
        card.summary,
        card.content_md
    )
    .trim_end()
    .to_string()
}

fn file_slug(title: &str, id: &str) -> String {
    let slug = title
        .to_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    format!("{}-{}.md", slug, &id[..8.min(id.len())])
}

#[derive(Debug, Clone, Copy)]
enum Kind {
    Cards,
    Drafts,
}

fn knowledge_repo_bucket(
    repo_owner: Option<&String>,
    repo_name: Option<&String>,
    kind: Kind,
) -> Result<PathBuf> {
    let owner = repo_owner
        .map(String::as_str)
        .context("knowledge entry is missing repo_owner provenance")?;
    let repo = repo_name
        .map(String::as_str)
        .context("knowledge entry is missing repo_name provenance")?;
    match kind {
        Kind::Cards => config::knowledge_cards_dir(owner, repo),
        Kind::Drafts => config::knowledge_drafts_dir(owner, repo),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    #[test]
    fn synthesized_context_contains_card_titles() {
        let store = Store::open_in_memory().unwrap();
        let project = store
            .create_project("proj", "/tmp/proj", "main", true)
            .unwrap();
        let draft = store
            .create_knowledge_draft(
                &project.id,
                None,
                None,
                None,
                Some("owner"),
                Some("repo"),
                "Rust conventions",
                "Prefer borrowing",
                "Use &str over String where possible.",
                Some(0.8),
                &[String::from("rust")],
                None,
                None,
                None,
            )
            .unwrap();
        let _card = store
            .promote_knowledge_draft(&draft.id, None, None, None)
            .unwrap();
        let context = synthesize_context_markdown(&store, &project.id).unwrap();
        assert!(context.contains("Rust conventions"));
    }
}
