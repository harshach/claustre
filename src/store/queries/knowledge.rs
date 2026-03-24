//! Knowledge draft/card persistence.

use anyhow::{Context, Result};
use rusqlite::params;
use uuid::Uuid;

use crate::store::Store;
use crate::store::models::{KnowledgeCard, KnowledgeDraft, KnowledgeLink};

use super::optional;

const KNOWLEDGE_CARD_COLUMNS: &str = "\
    id, project_id, github_item_id, thread_id, run_id, repo_owner, repo_name, title, summary, \
    content_md, confidence, tags_json, source_commit, source_branch, extractor, created_at, \
    accepted_at";

const KNOWLEDGE_DRAFT_COLUMNS: &str = "\
    id, project_id, github_item_id, thread_id, run_id, repo_owner, repo_name, title, summary, \
    content_md, confidence, tags_json, source_commit, source_branch, extractor, created_at, \
    promoted_card_id";

const KNOWLEDGE_LINK_COLUMNS: &str =
    "id, source_type, source_id, target_type, target_id, relation, created_at";

impl Store {
    #[expect(
        clippy::too_many_arguments,
        reason = "knowledge entries keep provenance inline"
    )]
    pub fn create_knowledge_draft(
        &self,
        project_id: &str,
        github_item_id: Option<&str>,
        thread_id: Option<&str>,
        run_id: Option<&str>,
        repo_owner: Option<&str>,
        repo_name: Option<&str>,
        title: &str,
        summary: &str,
        content_md: &str,
        confidence: Option<f64>,
        tags: &[String],
        source_commit: Option<&str>,
        source_branch: Option<&str>,
        extractor: Option<&str>,
    ) -> Result<KnowledgeDraft> {
        let id = Uuid::new_v4().to_string();
        let tags_json = serde_json::to_string(tags)?;
        self.conn.execute(
            "INSERT INTO knowledge_drafts (
                id, project_id, github_item_id, thread_id, run_id, repo_owner, repo_name, title,
                summary, content_md, confidence, tags_json, source_commit, source_branch,
                extractor
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                id,
                project_id,
                github_item_id,
                thread_id,
                run_id,
                repo_owner,
                repo_name,
                title,
                summary,
                content_md,
                confidence,
                tags_json,
                source_commit,
                source_branch,
                extractor
            ],
        )?;
        self.get_knowledge_draft(&id)
    }

    pub fn get_knowledge_draft(&self, id: &str) -> Result<KnowledgeDraft> {
        let sql = format!("SELECT {KNOWLEDGE_DRAFT_COLUMNS} FROM knowledge_drafts WHERE id = ?1");
        self.conn
            .query_row(&sql, params![id], Self::row_to_knowledge_draft)
            .with_context(|| format!("failed to fetch knowledge draft '{id}'"))
    }

    pub fn list_knowledge_drafts_for_project(
        &self,
        project_id: &str,
    ) -> Result<Vec<KnowledgeDraft>> {
        let sql = format!(
            "SELECT {KNOWLEDGE_DRAFT_COLUMNS} FROM knowledge_drafts WHERE project_id = ?1 ORDER BY created_at DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let items = stmt
            .query_map(params![project_id], Self::row_to_knowledge_draft)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(items)
    }

    pub fn promote_knowledge_draft(
        &self,
        draft_id: &str,
        maybe_title: Option<&str>,
        maybe_summary: Option<&str>,
        maybe_content_md: Option<&str>,
    ) -> Result<KnowledgeCard> {
        let draft = self.get_knowledge_draft(draft_id)?;
        let card_id = Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO knowledge_cards (
                id, project_id, github_item_id, thread_id, run_id, repo_owner, repo_name, title,
                summary, content_md, confidence, tags_json, source_commit, source_branch,
                extractor, accepted_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                card_id,
                draft.project_id,
                draft.github_item_id,
                draft.thread_id,
                draft.run_id,
                draft.repo_owner,
                draft.repo_name,
                maybe_title.unwrap_or(&draft.title),
                maybe_summary.unwrap_or(&draft.summary),
                maybe_content_md.unwrap_or(&draft.content_md),
                draft.confidence,
                serde_json::to_string(&draft.tags)?,
                draft.source_commit,
                draft.source_branch,
                draft.extractor,
                chrono::Utc::now().to_rfc3339()
            ],
        )?;
        self.conn.execute(
            "UPDATE knowledge_drafts SET promoted_card_id = ?1 WHERE id = ?2",
            params![card_id, draft_id],
        )?;
        self.get_knowledge_card(&card_id)
    }

    pub fn get_knowledge_card(&self, id: &str) -> Result<KnowledgeCard> {
        let sql = format!("SELECT {KNOWLEDGE_CARD_COLUMNS} FROM knowledge_cards WHERE id = ?1");
        self.conn
            .query_row(&sql, params![id], Self::row_to_knowledge_card)
            .with_context(|| format!("failed to fetch knowledge card '{id}'"))
    }

    pub fn list_knowledge_cards_for_project(&self, project_id: &str) -> Result<Vec<KnowledgeCard>> {
        let sql = format!(
            "SELECT {KNOWLEDGE_CARD_COLUMNS} FROM knowledge_cards WHERE project_id = ?1 ORDER BY accepted_at DESC, created_at DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let items = stmt
            .query_map(params![project_id], Self::row_to_knowledge_card)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(items)
    }

    pub fn create_knowledge_link(
        &self,
        source_type: &str,
        source_id: &str,
        target_type: &str,
        target_id: &str,
        relation: &str,
    ) -> Result<KnowledgeLink> {
        let id = Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO knowledge_links (id, source_type, source_id, target_type, target_id, relation)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, source_type, source_id, target_type, target_id, relation],
        )?;
        self.get_knowledge_link(&id)
    }

    pub fn get_knowledge_link(&self, id: &str) -> Result<KnowledgeLink> {
        let sql = format!("SELECT {KNOWLEDGE_LINK_COLUMNS} FROM knowledge_links WHERE id = ?1");
        self.conn
            .query_row(&sql, params![id], Self::row_to_knowledge_link)
            .with_context(|| format!("failed to fetch knowledge link '{id}'"))
    }

    pub fn find_knowledge_card_for_draft(&self, draft_id: &str) -> Result<Option<KnowledgeCard>> {
        let card_id: Option<String> = optional(self.conn.query_row(
            "SELECT promoted_card_id FROM knowledge_drafts WHERE id = ?1",
            params![draft_id],
            |row| row.get(0),
        ))?
        .flatten();
        card_id.map_or_else(|| Ok(None), |id| self.get_knowledge_card(&id).map(Some))
    }

    fn row_to_knowledge_card(row: &rusqlite::Row<'_>) -> rusqlite::Result<KnowledgeCard> {
        let tags_json: String = row.get(11)?;
        Ok(KnowledgeCard {
            id: row.get(0)?,
            project_id: row.get(1)?,
            github_item_id: row.get(2)?,
            thread_id: row.get(3)?,
            run_id: row.get(4)?,
            repo_owner: row.get(5)?,
            repo_name: row.get(6)?,
            title: row.get(7)?,
            summary: row.get(8)?,
            content_md: row.get(9)?,
            confidence: row.get(10)?,
            tags: serde_json::from_str(&tags_json).unwrap_or_default(),
            source_commit: row.get(12)?,
            source_branch: row.get(13)?,
            extractor: row.get(14)?,
            created_at: row.get(15)?,
            accepted_at: row.get(16)?,
        })
    }

    fn row_to_knowledge_draft(row: &rusqlite::Row<'_>) -> rusqlite::Result<KnowledgeDraft> {
        let tags_json: String = row.get(11)?;
        Ok(KnowledgeDraft {
            id: row.get(0)?,
            project_id: row.get(1)?,
            github_item_id: row.get(2)?,
            thread_id: row.get(3)?,
            run_id: row.get(4)?,
            repo_owner: row.get(5)?,
            repo_name: row.get(6)?,
            title: row.get(7)?,
            summary: row.get(8)?,
            content_md: row.get(9)?,
            confidence: row.get(10)?,
            tags: serde_json::from_str(&tags_json).unwrap_or_default(),
            source_commit: row.get(12)?,
            source_branch: row.get(13)?,
            extractor: row.get(14)?,
            created_at: row.get(15)?,
            promoted_card_id: row.get(16)?,
        })
    }

    fn row_to_knowledge_link(row: &rusqlite::Row<'_>) -> rusqlite::Result<KnowledgeLink> {
        Ok(KnowledgeLink {
            id: row.get(0)?,
            source_type: row.get(1)?,
            source_id: row.get(2)?,
            target_type: row.get(3)?,
            target_id: row.get(4)?,
            relation: row.get(5)?,
            created_at: row.get(6)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::store::Store;

    #[test]
    fn draft_promotion_creates_card_and_back_link() {
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
                "Card",
                "summary",
                "# content",
                Some(0.9),
                &[String::from("tag")],
                Some("abc123"),
                Some("feat"),
                Some("extractor"),
            )
            .unwrap();

        let card = store
            .promote_knowledge_draft(&draft.id, None, None, None)
            .unwrap();
        assert_eq!(card.title, "Card");
        let found = store
            .find_knowledge_card_for_draft(&draft.id)
            .unwrap()
            .unwrap();
        assert_eq!(found.id, card.id);
    }
}
