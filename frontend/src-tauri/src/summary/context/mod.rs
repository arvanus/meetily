//! Per-meeting context for AI summary generation.
//!
//! Owns the textarea content (`meeting_summary_context.context_prompt`) and
//! the text-file attachments (`meeting_context_attachments`). At summary
//! generation time, the loaded context is injected into the LLM user prompt.

pub mod commands;
pub mod prompt_builder;
pub mod repository;
pub mod storage;
pub mod types;
