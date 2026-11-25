use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// 最近一张已成功落盘归档的卡片元数据，用于支持 Alt+Up 唤回原地修订
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LastArchivedCard {
    pub file_path: PathBuf,
    pub id: String,
    pub created: String,
    pub raw_body: String,
    pub target_index: usize,
}

/// 持久化到 %LOCALAPPDATA%\ZettelQuick\draft.json 的防崩溃恢复快照
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CrashDraftSnapshot {
    pub content: String,
    pub target_index: usize,
    #[serde(default)]
    pub pasted_images: Vec<PathBuf>,
}

/// 内存态草稿与修订会话状态机
#[derive(Debug, Default)]
pub struct DraftSession {
    pub content: String,
    pub target_index: usize,
    pub session_pasted_images: Vec<PathBuf>,
    pub active_recall: Option<LastArchivedCard>,
    pub last_archived: Option<LastArchivedCard>,
    draft_file: PathBuf,
}

impl DraftSession {
    pub fn new(draft_file: PathBuf) -> Self {
        let mut session = Self {
            draft_file,
            ..Default::default()
        };
        session.restore_crash_snapshot_if_any();
        session
    }

    fn restore_crash_snapshot_if_any(&mut self) {
        if !self.draft_file.exists() {
            return;
        }
        match fs::read_to_string(&self.draft_file) {
            Ok(raw) => match serde_json::from_str::<CrashDraftSnapshot>(&raw) {
                Ok(snap) if !snap.content.trim().is_empty() => {
                    crate::log_info!(
                        "DRAFT",
                        "Restored unsaved crash draft from '{}': chars={}, target_index={}, pasted_images={}",
                        self.draft_file.display(),
                        snap.content.chars().count(),
                        snap.target_index,
                        snap.pasted_images.len()
                    );
                    self.content = snap.content;
                    self.target_index = snap.target_index;
                    self.session_pasted_images = snap.pasted_images;
                    return;
                }
                Ok(_) => {}
                Err(e) => {
                    crate::log_warn!(
                        "DRAFT",
                        "Invalid crash draft snapshot '{}': {e} -> cleaning up",
                        self.draft_file.display()
                    );
                }
            },
            Err(e) => {
                crate::log_warn!(
                    "DRAFT",
                    "Failed reading crash draft snapshot '{}': {e}",
                    self.draft_file.display()
                );
            }
        }
        let _ = fs::remove_file(&self.draft_file);
    }

    pub fn persist_snapshot(&self) {
        if self.content.trim().is_empty() && self.active_recall.is_none() {
            let _ = fs::remove_file(&self.draft_file);
            return;
        }
        // 仅在普通草稿非空时写防崩溃快照；修订上一张模式下原文件已在磁盘中安全存在
        if self.active_recall.is_none() {
            let snap = CrashDraftSnapshot {
                content: self.content.clone(),
                target_index: self.target_index,
                pasted_images: self.session_pasted_images.clone(),
            };
            if let Ok(json) = serde_json::to_string(&snap) {
                if let Some(parent) = self.draft_file.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                if let Err(e) = fs::write(&self.draft_file, json) {
                    crate::log_warn!(
                        "DRAFT",
                        "Failed writing crash snapshot '{}': {e}",
                        self.draft_file.display()
                    );
                } else {
                    crate::log_debug!(
                        "DRAFT",
                        "Synced crash snapshot: chars={}, target_index={}",
                        self.content.chars().count(),
                        self.target_index
                    );
                }
            }
        }
    }

    pub fn update_content(&mut self, content: String, target_index: usize) {
        self.content = content;
        self.target_index = target_index;
        self.persist_snapshot();
    }

    pub fn record_pasted_image(&mut self, img_path: PathBuf) {
        self.session_pasted_images.push(img_path);
        self.persist_snapshot();
    }

    /// 开始修订最近一张已保存的卡片（要求当前草稿为空且原文件仍存在）
    pub fn begin_recall_last(&mut self) -> Result<LastArchivedCard, String> {
        if !self.content.trim().is_empty() {
            return Err("当前草稿非空，仅在空白编辑框下可唤回上一张卡片".to_string());
        }
        if let Some(active) = &self.active_recall {
            return Ok(active.clone());
        }
        let last = self
            .last_archived
            .clone()
            .ok_or_else(|| "本次会话暂无可唤回的上一张卡片".to_string())?;

        if !last.file_path.exists() {
            crate::log_warn!(
                "DRAFT",
                "Recall requested, but previous card '{}' no longer exists on disk",
                last.file_path.display()
            );
            self.last_archived = None;
            return Err("上一张卡片文件在磁盘上已被移动或删除".to_string());
        }

        crate::log_info!(
            "DRAFT",
            "Entered recall-last mode: id='{}', file='{}', chars={}",
            last.id,
            last.file_path.display(),
            last.raw_body.chars().count()
        );
        self.content = last.raw_body.clone();
        self.target_index = last.target_index;
        self.active_recall = Some(last.clone());
        Ok(last)
    }

    /// 正常归档完成后重置草稿，不删除已落盘引用的截图
    pub fn complete_archive(&mut self, saved: Option<LastArchivedCard>) {
        self.content.clear();
        self.session_pasted_images.clear();
        self.active_recall = None;
        self.last_archived = saved;
        let _ = fs::remove_file(&self.draft_file);
    }

    /// 主动放弃当前草稿（Ctrl+Shift+Backspace）：清理本次草稿未被引用的孤儿截图，保留已存上一张卡不动
    pub fn discard_current_draft(&mut self) {
        let orphan_count = self.session_pasted_images.len();
        let was_recalling = self.active_recall.is_some();
        let discarded_chars = self.content.chars().count();
        for orphan in self.session_pasted_images.drain(..) {
            remove_if_exists(&orphan);
        }
        self.content.clear();
        self.active_recall = None;
        let _ = fs::remove_file(&self.draft_file);
        if discarded_chars > 0 || orphan_count > 0 || was_recalling {
            crate::log_info!(
                "DRAFT",
                "Discarded draft: chars={discarded_chars}, orphan_images_deleted={orphan_count}, cancelled_recall={was_recalling}"
            );
        }
    }
}

fn remove_if_exists(path: &Path) {
    if path.exists() {
        let _ = fs::remove_file(path);
    }
}
