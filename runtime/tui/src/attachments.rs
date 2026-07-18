use std::{
    fs,
    path::{Path, PathBuf},
};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use openagent_bridge_server_client::RemoteTurnAttachment;

use crate::{TerminalEventHandler, TimelineLine, TuiState};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ComposerFileCandidate {
    pub reference: String,
    pub kind: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FilePickerState {
    pub query: String,
    pub selected: usize,
    pub candidates: Vec<ComposerFileCandidate>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ExpandedPrompt {
    pub(crate) prompt: String,
    pub(crate) attachments: Vec<RemoteTurnAttachment>,
    pub(crate) lines: Vec<TimelineLine>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FileAttachmentRef {
    query: String,
    range: Option<(usize, usize)>,
}

pub(crate) fn expand_file_attachments(workspace: &Path, prompt: &str) -> ExpandedPrompt {
    let refs = prompt
        .split_whitespace()
        .filter_map(parse_file_attachment_ref)
        .take(8)
        .collect::<Vec<_>>();
    if refs.is_empty() {
        return ExpandedPrompt {
            prompt: prompt.to_string(),
            attachments: Vec::new(),
            lines: Vec::new(),
        };
    }
    let mut attachments = Vec::new();
    let mut lines = Vec::new();
    for reference in refs {
        let Some(path) = resolve_attachment_path(workspace, &reference.query) else {
            lines.push(TimelineLine::new(
                "warning",
                format!("attachment not found: @{}", reference.query),
                true,
            ));
            continue;
        };
        match render_attachment(workspace, &path, reference.range) {
            Ok(section) => {
                attachments.push(section.attachment);
                lines.push(TimelineLine::new(
                    "status",
                    format!("attached {}", section.label),
                    false,
                ));
            }
            Err(error) => lines.push(TimelineLine::new("warning", error, true)),
        }
    }
    ExpandedPrompt {
        prompt: prompt.to_string(),
        attachments,
        lines,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AttachmentSection {
    label: String,
    attachment: RemoteTurnAttachment,
}

fn parse_file_attachment_ref(token: &str) -> Option<FileAttachmentRef> {
    let raw = token.strip_prefix('@')?;
    if raw.is_empty() || raw.starts_with('@') {
        return None;
    }
    let trimmed = raw.trim_matches(|ch: char| matches!(ch, ',' | ';' | ')' | '(' | '"' | '\''));
    if trimmed.is_empty() || trimmed.contains("://") {
        return None;
    }
    let (query, range) = if let Some((path, suffix)) = trimmed.split_once("#L") {
        (path.to_string(), parse_line_range(suffix))
    } else if let Some((path, suffix)) = trimmed.rsplit_once(':') {
        if suffix.chars().all(|ch| ch.is_ascii_digit() || ch == '-')
            && suffix.chars().any(|ch| ch.is_ascii_digit())
        {
            (path.to_string(), parse_line_range(suffix))
        } else {
            (trimmed.to_string(), None)
        }
    } else {
        (trimmed.to_string(), None)
    };
    (!query.is_empty()).then_some(FileAttachmentRef { query, range })
}

fn parse_line_range(value: &str) -> Option<(usize, usize)> {
    let (start, end) = value
        .split_once('-')
        .map_or((value, value), |(start, end)| (start, end));
    let start = start.parse::<usize>().ok()?.max(1);
    let end = end.parse::<usize>().ok().unwrap_or(start).max(start);
    Some((start, end.min(start + 400)))
}

fn resolve_attachment_path(workspace: &Path, query: &str) -> Option<PathBuf> {
    let raw = PathBuf::from(query);
    let exact = if raw.is_absolute() {
        raw
    } else {
        workspace.join(&raw)
    };
    if exact.is_file() {
        return Some(exact);
    }
    fuzzy_find_files(workspace, query, 1)
        .into_iter()
        .next()
        .map(|item| item.path)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FilePickerMatch {
    pub(crate) path: PathBuf,
    pub(crate) reference: String,
    pub(crate) score: usize,
}

pub(crate) fn fuzzy_find_files(
    workspace: &Path,
    query: &str,
    limit: usize,
) -> Vec<FilePickerMatch> {
    let query = query
        .trim()
        .trim_start_matches('@')
        .trim_matches(|ch: char| matches!(ch, '"' | '\'' | '`'))
        .to_ascii_lowercase();
    let mut stack = vec![workspace.to_path_buf()];
    let mut matches = Vec::new();
    let mut visited = 0_usize;
    while let Some(path) = stack.pop() {
        visited += 1;
        if visited > 5000 {
            break;
        }
        if path.is_dir() {
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default();
            if matches!(
                name,
                ".git" | "node_modules" | "target" | "dist" | "build" | ".venv" | "__pycache__"
            ) {
                continue;
            }
            if let Ok(entries) = fs::read_dir(&path) {
                for entry in entries.flatten() {
                    stack.push(entry.path());
                }
            }
            continue;
        }
        if !path.is_file() {
            continue;
        }
        let relative = relative_display_path(workspace, &path);
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        if let Some(score) = fuzzy_file_score(&relative, name, &query) {
            let Some(reference) = normalize_attachment_reference_token(&relative) else {
                continue;
            };
            matches.push(FilePickerMatch {
                path,
                reference,
                score,
            });
        }
    }
    matches.sort_by(|left, right| {
        left.score
            .cmp(&right.score)
            .then_with(|| left.reference.len().cmp(&right.reference.len()))
            .then_with(|| left.reference.cmp(&right.reference))
    });
    matches.truncate(limit);
    matches
}

pub(crate) fn composer_candidate_from_match(item: FilePickerMatch) -> ComposerFileCandidate {
    let kind = if is_image_path(&item.path) {
        "image"
    } else {
        "file"
    };
    ComposerFileCandidate {
        reference: item.reference,
        kind: kind.to_string(),
    }
}

fn fuzzy_file_score(relative: &str, name: &str, query: &str) -> Option<usize> {
    let relative = relative.to_ascii_lowercase();
    let name = name.to_ascii_lowercase();
    if query.is_empty() {
        return Some(100 + relative.matches('/').count());
    }
    if relative == query {
        Some(0)
    } else if name == query {
        Some(1)
    } else if relative.ends_with(query) {
        Some(2)
    } else if name.contains(query) {
        Some(3)
    } else if relative.contains(query) {
        Some(4)
    } else if fuzzy_subsequence(&relative, query) {
        Some(10 + relative.len().saturating_sub(query.len()))
    } else {
        None
    }
}

fn fuzzy_subsequence(value: &str, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let mut query_chars = query.chars();
    let Some(mut expected) = query_chars.next() else {
        return true;
    };
    for ch in value.chars() {
        if ch == expected {
            match query_chars.next() {
                Some(next) => expected = next,
                None => return true,
            }
        }
    }
    false
}

pub(crate) fn attachment_reference_from_parts(
    path: &str,
    line: Option<usize>,
    start: Option<usize>,
    end: Option<usize>,
) -> String {
    let mut reference = path.trim().trim_start_matches('@').to_string();
    if let Some(line) = line.filter(|value| *value > 0) {
        reference.push_str(&format!(":{line}"));
    } else if let Some(start) = start.filter(|value| *value > 0) {
        let end = end.unwrap_or(start).max(start);
        if end == start {
            reference.push_str(&format!(":{start}"));
        } else {
            reference.push_str(&format!(":{start}-{end}"));
        }
    }
    reference
}

pub(crate) fn normalize_attachment_reference_token(reference: &str) -> Option<String> {
    let reference = reference
        .trim()
        .trim_start_matches('@')
        .trim_matches(|ch: char| matches!(ch, ',' | ';' | ')' | '(' | '"' | '\'' | '`'));
    if reference.is_empty()
        || reference.contains("://")
        || reference.chars().any(char::is_whitespace)
    {
        return None;
    }
    Some(format!("@{reference}"))
}

fn render_attachment(
    workspace: &Path,
    path: &Path,
    range: Option<(usize, usize)>,
) -> Result<AttachmentSection, String> {
    let label = match range {
        Some((start, end)) if start == end => {
            format!("{}:{start}", relative_display_path(workspace, path))
        }
        Some((start, end)) => format!("{}:{start}-{end}", relative_display_path(workspace, path)),
        None => relative_display_path(workspace, path),
    };
    if is_image_path(path) {
        let bytes = fs::metadata(path).map_err(|error| error.to_string())?.len();
        return Ok(AttachmentSection {
            label: label.clone(),
            attachment: RemoteTurnAttachment::new(
                "image",
                label,
                path.file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or("image"),
                bytes,
                image_content_type(path),
                "",
            ),
        });
    }
    let mut content = fs::read_to_string(path)
        .map_err(|error| format!("failed to attach {}: {error}", path.display()))?;
    if let Some((start, end)) = range {
        let lines = content.lines().collect::<Vec<_>>();
        let start_index = start.saturating_sub(1).min(lines.len());
        let end_index = end.min(lines.len());
        content = lines[start_index..end_index].join("\n");
    }
    if content.len() > 24_000 {
        let mut end = 24_000.min(content.len());
        while end > 0 && !content.is_char_boundary(end) {
            end -= 1;
        }
        content.truncate(end);
        content.push_str("\n... attachment truncated ...");
    }
    Ok(AttachmentSection {
        label: label.clone(),
        attachment: RemoteTurnAttachment::new(
            "file",
            label,
            path.file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("attachment"),
            content.len() as u64,
            "text/plain",
            content,
        ),
    })
}

fn image_content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        _ => "image/png",
    }
}

pub(crate) fn is_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .map(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg"
            )
        })
        .unwrap_or(false)
}

fn relative_display_path(workspace: &Path, path: &Path) -> String {
    path.strip_prefix(workspace)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

pub(crate) fn handle_file_picker_key<H: TerminalEventHandler>(
    key: KeyEvent,
    state: &mut TuiState,
    handler: &mut H,
) -> Result<(), String> {
    match key.code {
        KeyCode::Esc => {
            state.close_file_picker();
        }
        KeyCode::Enter => {
            state.insert_selected_file_picker_reference();
        }
        KeyCode::Up => {
            state.file_picker_previous();
        }
        KeyCode::Down | KeyCode::Tab => {
            state.file_picker_next();
        }
        KeyCode::Backspace => {
            if let Some(picker) = state.file_picker.as_mut() {
                picker.query.pop();
            }
            if let Err(error) = refresh_file_picker_from_handler(state, handler) {
                state
                    .timeline
                    .push(TimelineLine::new("warning", error, true));
                state.status = "file picker refresh failed".to_string();
            }
        }
        KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(picker) = state.file_picker.as_mut() {
                picker.query.push(ch);
            }
            if let Err(error) = refresh_file_picker_from_handler(state, handler) {
                state
                    .timeline
                    .push(TimelineLine::new("warning", error, true));
                state.status = "file picker refresh failed".to_string();
            }
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn open_file_picker_from_handler<H: TerminalEventHandler>(
    state: &mut TuiState,
    handler: &mut H,
    query: &str,
) -> Result<(), String> {
    let candidates = handler.search_files(query)?;
    state.open_file_picker(query, candidates);
    Ok(())
}

fn refresh_file_picker_from_handler<H: TerminalEventHandler>(
    state: &mut TuiState,
    handler: &mut H,
) -> Result<(), String> {
    let query = state
        .file_picker
        .as_ref()
        .map(|picker| picker.query.clone())
        .unwrap_or_default();
    let candidates = handler.search_files(&query)?;
    if let Some(picker) = state.file_picker.as_mut() {
        picker.candidates = candidates;
        picker.selected = picker
            .selected
            .min(picker.candidates.len().saturating_sub(1));
    }
    state.status = "file picker".to_string();
    Ok(())
}

impl TuiState {
    pub fn open_file_picker(&mut self, query: &str, candidates: Vec<ComposerFileCandidate>) {
        self.model_picker = None;
        self.agent_picker = None;
        self.choice_picker = None;
        self.session_picker = None;
        self.file_picker = Some(FilePickerState {
            query: query.trim().to_string(),
            selected: 0,
            candidates,
        });
        self.status = "file picker".to_string();
    }

    pub fn close_file_picker(&mut self) {
        self.file_picker = None;
        self.status = "file picker closed".to_string();
    }

    pub fn file_picker_previous(&mut self) {
        let Some(picker) = self.file_picker.as_mut() else {
            return;
        };
        picker.selected = picker.selected.saturating_sub(1);
        self.status = "file picker".to_string();
    }

    pub fn file_picker_next(&mut self) {
        let Some(picker) = self.file_picker.as_mut() else {
            return;
        };
        if !picker.candidates.is_empty() {
            picker.selected = (picker.selected + 1).min(picker.candidates.len() - 1);
        }
        self.status = "file picker".to_string();
    }

    pub fn insert_selected_file_picker_reference(&mut self) {
        let Some(reference) = self.file_picker.as_ref().and_then(|picker| {
            picker
                .candidates
                .get(picker.selected)
                .map(|candidate| candidate.reference.clone())
        }) else {
            self.status = "file picker empty".to_string();
            return;
        };
        self.file_picker = None;
        self.insert_attachment_reference(&reference);
    }
}
