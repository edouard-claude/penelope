//! Outils natifs, une fonction par famille ; `native` aiguille un outil déjà validé.

use super::*;

mod fs_shell;
mod git_http;
mod memory;
mod misc;
mod schedules_messaging;
mod self_tools;
mod skills_workflows;

impl NativeToolExecutor {
    /// Exécute un outil natif dont la spécification et les arguments sont validés.
    pub(super) async fn native(
        &self,
        name: &str,
        args: &Value,
        cancel: &penelope_llm::CancelToken,
    ) -> ToolResult<ToolOutcome> {
        match name {
            "fs_read" | "fs_list" | "fs_search" | "fs_write" | "fs_edit" => {
                self.fs_tools(name, args, cancel).await
            }
            "shell_exec" => self.shell_tools(name, args, cancel).await,
            "git_status" | "git_diff" | "git_branch" | "git_commit" | "git_clone" | "git_push" => {
                self.git_tools(name, args, cancel).await
            }
            "http_fetch" => self.http_tools(name, args, cancel).await,
            "self_status" | "self_docs" | "config_set" | "time_now" => {
                self.self_tools(name, args, cancel).await
            }
            "schedule_create" | "schedule_list" | "schedule_move" | "schedule_delete" => {
                self.schedule_tools(name, args, cancel).await
            }
            "send_message" | "send_voice" | "send_file" => {
                self.message_tools(name, args, cancel).await
            }
            "mem_search" | "mem_neighbors" | "mem_get" | "mem_note" | "mem_remember"
            | "mem_forget" => self.memory_tools(name, args, cancel).await,
            "intent_create" | "intent_list" | "intent_cancel" => {
                self.intent_tools(name, args, cancel).await
            }
            "history_grep"
            | "history_describe"
            | "history_expand"
            | "history_expand_query"
            | "artifact_read" => self.history_tools(name, args, cancel).await,
            "skill_search" | "skill_load" | "skill_propose" | "skill_patch" => {
                self.skill_tools(name, args, cancel).await
            }
            "workflow_list" | "workflow_describe" | "workflow_plan" | "workflow_start"
            | "workflow_status" | "workflow_control" | "workflow_author" | "sub_agent_spawn" => {
                self.workflow_tools(name, args, cancel).await
            }
            "job_status" | "job_wait" | "job_cancel" | "job_list" | "session_notes"
            | "session_metadata" | "ask_user" | "step_done" | "return_value" | "image_inspect"
            | "image_generate" => self.misc_tools(name, args, cancel).await,
            other => Err(ToolError::Unknown(other.to_string())),
        }
    }
}
