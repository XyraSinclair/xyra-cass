use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, Local, NaiveDate, Utc};
use clap::ValueEnum;
use regex::RegexBuilder;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};
use walkdir::{DirEntry, WalkDir};

const DEFAULT_LIMIT: usize = 20;
const DEFAULT_MAX_FILES: usize = 2_000;
const DEFAULT_MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
const DEFAULT_MAX_HITS_PER_FILE: usize = 3;
const DEFAULT_TIMEOUT: Duration = Duration::from_millis(3_000);
const DEFAULT_RECENT_WINDOW: Duration = Duration::from_secs(14 * 24 * 60 * 60);
const MAX_SNIPPET_CHARS: usize = 500;

#[derive(Debug, Clone)]
pub struct LiveGrepOptions {
    pub query: String,
    pub roots: Vec<PathBuf>,
    pub agents: Vec<String>,
    pub limit: usize,
    pub max_files: usize,
    pub max_file_bytes: u64,
    pub max_hits_per_file: usize,
    pub timeout: Duration,
    pub include_compacted: bool,
    pub since: Option<SystemTime>,
    pub until: Option<SystemTime>,
    pub time_filter_label: Option<String>,
    pub role: RoleFilter,
    pub order: ScanOrder,
    pub ignore_case: bool,
    pub regex: bool,
}

impl LiveGrepOptions {
    pub fn bounded(query: String) -> Self {
        Self {
            query,
            roots: default_session_roots(),
            agents: Vec::new(),
            limit: DEFAULT_LIMIT,
            max_files: DEFAULT_MAX_FILES,
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            max_hits_per_file: DEFAULT_MAX_HITS_PER_FILE,
            timeout: DEFAULT_TIMEOUT,
            include_compacted: false,
            since: SystemTime::now().checked_sub(DEFAULT_RECENT_WINDOW),
            until: None,
            time_filter_label: Some("last 14 days".to_string()),
            role: RoleFilter::Any,
            order: ScanOrder::Newest,
            ignore_case: true,
            regex: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, ValueEnum)]
pub enum RoleFilter {
    Any,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, ValueEnum)]
pub enum ScanOrder {
    Newest,
    Oldest,
}

#[derive(Debug, Clone, Serialize)]
pub struct LiveGrepHit {
    pub source_path: String,
    pub line_number: usize,
    pub agent: String,
    pub modified: Option<String>,
    pub snippet: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LiveGrepSession {
    pub source_path: String,
    pub agent: String,
    pub modified: Option<String>,
    pub hit_count: usize,
    pub first_line_number: usize,
    pub first_snippet: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DirectQueryPlan {
    pub intent: String,
    pub reason: String,
    pub engine: String,
    pub execution_path: String,
    pub provider_scope: Vec<String>,
    pub root_scope: Vec<String>,
    pub time_scope: String,
    pub max_files: usize,
    pub max_file_bytes: u64,
    pub max_hits_per_file: usize,
    pub limit: usize,
    pub role: RoleFilter,
    pub order: ScanOrder,
    pub regex: bool,
    pub case_sensitive: bool,
    pub timeout_ms: u128,
    pub budget: DirectQueryBudget,
    pub will_touch: Vec<String>,
    pub will_not_touch: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DirectQueryBudget {
    pub timeout_ms: u128,
    pub max_files: usize,
    pub max_bytes_per_file: u64,
    pub threads: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct LiveGrepMeta {
    pub intent: String,
    pub query_plan: DirectQueryPlan,
    pub touched_subsystems: Vec<String>,
    pub did_not_touch_subsystems: Vec<String>,
    pub candidate_files: usize,
    pub scanned_files: usize,
    pub skipped_files: usize,
    pub matched_files: usize,
    pub elapsed_ms: u128,
    pub timed_out: bool,
    pub limit: usize,
    pub max_files: usize,
    pub max_file_bytes: u64,
    pub max_hits_per_file: usize,
    pub role: RoleFilter,
    pub include_compacted: bool,
    pub order: ScanOrder,
    pub time_filter: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LiveGrepResult {
    pub query: String,
    pub hits: Vec<LiveGrepHit>,
    pub sessions: Vec<LiveGrepSession>,
    pub roots: Vec<String>,
    pub _meta: LiveGrepMeta,
}

pub fn live_grep(opts: &LiveGrepOptions) -> Result<LiveGrepResult> {
    if opts.query.trim().is_empty() {
        anyhow::bail!("live grep query must not be empty");
    }

    let started = Instant::now();
    let deadline = started + opts.timeout;
    let mut candidates = collect_candidate_files(opts, deadline)?;
    sort_candidates(&mut candidates, opts.order);

    let matcher = Matcher::new(&opts.query, opts.ignore_case, opts.regex)?;
    let limit = if opts.limit == 0 {
        DEFAULT_LIMIT
    } else {
        opts.limit
    };
    let max_hits_per_file = opts.max_hits_per_file;
    let mut scanned_files = 0usize;
    let mut skipped_files = 0usize;
    let mut hits = Vec::new();
    let mut timed_out = false;
    let candidate_files = candidates.len();

    for candidate in candidates.into_iter().take(opts.max_files) {
        if Instant::now() >= deadline {
            timed_out = true;
            break;
        }
        if candidate.size_bytes > opts.max_file_bytes {
            skipped_files = skipped_files.saturating_add(1);
            continue;
        }
        scanned_files = scanned_files.saturating_add(1);
        let file = match File::open(&candidate.path) {
            Ok(file) => file,
            Err(_) => {
                skipped_files = skipped_files.saturating_add(1);
                continue;
            }
        };
        let reader = BufReader::new(file);
        let mut file_hits = 0usize;
        for (index, line) in reader.lines().enumerate() {
            if Instant::now() >= deadline {
                timed_out = true;
                break;
            }
            let Ok(line) = line else {
                continue;
            };
            if !opts.include_compacted && is_compacted_history_line(&line) {
                continue;
            }
            if !line_matches_role(&line, opts.role) || !matcher.matches(&line) {
                continue;
            }
            hits.push(LiveGrepHit {
                source_path: candidate.path.to_string_lossy().into_owned(),
                line_number: index.saturating_add(1),
                agent: candidate.agent.clone(),
                modified: candidate.modified.map(format_system_time),
                snippet: trim_snippet(&line),
            });
            file_hits = file_hits.saturating_add(1);
            if hits.len() >= limit || (max_hits_per_file > 0 && file_hits >= max_hits_per_file) {
                break;
            }
        }
        if hits.len() >= limit || timed_out {
            break;
        }
    }

    let sessions = summarize_sessions(&hits);
    let matched_files = sessions.len();
    let touched_subsystems = direct_touched_subsystems();
    let did_not_touch_subsystems = direct_did_not_touch_subsystems();

    Ok(LiveGrepResult {
        query: opts.query.clone(),
        sessions,
        hits,
        roots: opts
            .roots
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect(),
        _meta: LiveGrepMeta {
            intent: "direct_find".to_string(),
            query_plan: DirectQueryPlan {
                intent: "direct_find".to_string(),
                reason: "source_log_first_exact_or_phrase_recovery".to_string(),
                engine: "live_grep".to_string(),
                execution_path: "bounded-filesystem-scan".to_string(),
                provider_scope: if opts.agents.is_empty() {
                    vec!["all-detected-session-files".to_string()]
                } else {
                    opts.agents.clone()
                },
                root_scope: opts
                    .roots
                    .iter()
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect(),
                time_scope: opts
                    .time_filter_label
                    .clone()
                    .unwrap_or_else(|| "all".to_string()),
                max_files: opts.max_files,
                max_file_bytes: opts.max_file_bytes,
                max_hits_per_file,
                limit,
                role: opts.role,
                order: opts.order,
                regex: opts.regex,
                case_sensitive: !opts.ignore_case,
                timeout_ms: opts.timeout.as_millis(),
                budget: DirectQueryBudget {
                    timeout_ms: opts.timeout.as_millis(),
                    max_files: opts.max_files,
                    max_bytes_per_file: opts.max_file_bytes,
                    threads: 1,
                },
                will_touch: touched_subsystems.clone(),
                will_not_touch: did_not_touch_subsystems.clone(),
            },
            touched_subsystems,
            did_not_touch_subsystems,
            candidate_files,
            scanned_files,
            skipped_files,
            matched_files,
            elapsed_ms: started.elapsed().as_millis(),
            timed_out,
            limit,
            max_files: opts.max_files,
            max_file_bytes: opts.max_file_bytes,
            max_hits_per_file,
            role: opts.role,
            include_compacted: opts.include_compacted,
            order: opts.order,
            time_filter: opts.time_filter_label.clone(),
        },
    })
}

fn direct_touched_subsystems() -> Vec<String> {
    vec!["source_files".to_string()]
}

fn direct_did_not_touch_subsystems() -> Vec<String> {
    [
        "sqlite",
        "tantivy",
        "semantic_vectors",
        "reranker",
        "daemon",
        "index_locks",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn sort_candidates(candidates: &mut [CandidateFile], order: ScanOrder) {
    match order {
        ScanOrder::Newest => candidates.sort_by(|left, right| {
            right
                .modified
                .cmp(&left.modified)
                .then_with(|| left.path.cmp(&right.path))
        }),
        ScanOrder::Oldest => candidates.sort_by(|left, right| {
            left.modified
                .cmp(&right.modified)
                .then_with(|| left.path.cmp(&right.path))
        }),
    }
}

fn summarize_sessions(hits: &[LiveGrepHit]) -> Vec<LiveGrepSession> {
    let mut session_indexes = BTreeMap::<String, usize>::new();
    let mut sessions = Vec::<LiveGrepSession>::new();
    for hit in hits {
        if let Some(index) = session_indexes.get(&hit.source_path).copied() {
            sessions[index].hit_count = sessions[index].hit_count.saturating_add(1);
        } else {
            let index = sessions.len();
            session_indexes.insert(hit.source_path.clone(), index);
            sessions.push(LiveGrepSession {
                source_path: hit.source_path.clone(),
                agent: hit.agent.clone(),
                modified: hit.modified.clone(),
                hit_count: 1,
                first_line_number: hit.line_number,
                first_snippet: hit.snippet.clone(),
            });
        }
    }
    sessions
}

fn is_compacted_history_line(line: &str) -> bool {
    line.contains("\"type\":\"compacted\"")
}

fn line_matches_role(line: &str, role: RoleFilter) -> bool {
    match role {
        RoleFilter::Any => true,
        RoleFilter::User => {
            line.contains("\"role\":\"user\"") || line.contains("\"type\":\"user_message\"")
        }
        RoleFilter::Assistant => {
            line.contains("\"role\":\"assistant\"")
                || line.contains("\"type\":\"assistant\"")
                || line.contains("\"type\":\"agent_message\"")
        }
        RoleFilter::Tool => {
            line.contains("\"type\":\"function_call\"")
                || line.contains("\"type\":\"function_call_output\"")
                || line.contains("\"type\":\"tool_use\"")
                || line.contains("\"type\":\"tool_result\"")
                || line.contains("\"name\":\"exec_command\"")
                || line.contains("\"name\":\"apply_patch\"")
        }
    }
}

#[derive(Debug, Clone)]
struct CandidateFile {
    path: PathBuf,
    agent: String,
    modified: Option<SystemTime>,
    size_bytes: u64,
}

fn collect_candidate_files(
    opts: &LiveGrepOptions,
    deadline: Instant,
) -> Result<Vec<CandidateFile>> {
    let mut files = Vec::new();
    let wanted_agents = opts
        .agents
        .iter()
        .map(|agent| agent.to_ascii_lowercase())
        .collect::<BTreeSet<_>>();

    for root in &opts.roots {
        if Instant::now() >= deadline {
            break;
        }
        if !root.exists() {
            continue;
        }
        if let Some(mut known_candidates) =
            collect_known_dated_candidates(root, opts, &wanted_agents, deadline)
        {
            files.append(&mut known_candidates);
            continue;
        }
        for entry in WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| !is_hidden_noise_dir(entry))
            .filter_map(|entry| entry.ok())
        {
            if Instant::now() >= deadline {
                break;
            }
            if !entry.file_type().is_file() || !is_session_file(entry.path()) {
                continue;
            }
            let agent = infer_agent(entry.path());
            if !wanted_agents.is_empty() && !wanted_agents.contains(&agent) {
                continue;
            }
            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };
            let modified = metadata.modified().ok();
            if !time_in_range(modified, opts.since, opts.until) {
                continue;
            }
            files.push(CandidateFile {
                path: entry.path().to_path_buf(),
                agent,
                modified,
                size_bytes: metadata.len(),
            });
        }
    }

    Ok(files)
}

fn collect_known_dated_candidates(
    root: &Path,
    opts: &LiveGrepOptions,
    wanted_agents: &BTreeSet<String>,
    deadline: Instant,
) -> Option<Vec<CandidateFile>> {
    let since = opts.since?;
    let home = dirs::home_dir()?;
    let codex_tabs_root = home.join(".codex/tabs");
    let codex_sessions_root = home.join(".codex/sessions");
    let dates = date_range_for_filter(since, opts.until)?;

    if root == codex_sessions_root {
        return Some(collect_dated_session_dir_candidates(
            root,
            &dates,
            "codex",
            wanted_agents,
            opts,
            deadline,
        ));
    }

    if root == codex_tabs_root {
        let mut files = Vec::new();
        let tab_dirs = match fs::read_dir(root) {
            Ok(tab_dirs) => tab_dirs,
            Err(_) => return Some(files),
        };
        for tab_dir in tab_dirs.filter_map(|entry| entry.ok()) {
            if Instant::now() >= deadline {
                break;
            }
            let tab_path = tab_dir.path();
            if !tab_path.is_dir() {
                continue;
            }
            push_candidate_if_session_file(
                &mut files,
                tab_path.join("history.jsonl"),
                "codex",
                wanted_agents,
                opts,
            );
            let sessions_root = tab_path.join("sessions");
            files.extend(collect_dated_session_dir_candidates(
                &sessions_root,
                &dates,
                "codex",
                wanted_agents,
                opts,
                deadline,
            ));
        }
        return Some(files);
    }

    None
}

fn collect_dated_session_dir_candidates(
    sessions_root: &Path,
    dates: &[NaiveDate],
    agent: &str,
    wanted_agents: &BTreeSet<String>,
    opts: &LiveGrepOptions,
    deadline: Instant,
) -> Vec<CandidateFile> {
    let mut files = Vec::new();
    for date in dates {
        if Instant::now() >= deadline {
            break;
        }
        let dir = sessions_root
            .join(format!("{:04}", date.year()))
            .join(format!("{:02}", date.month()))
            .join(format!("{:02}", date.day()));
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.filter_map(|entry| entry.ok()) {
            if Instant::now() >= deadline {
                break;
            }
            push_candidate_if_session_file(&mut files, entry.path(), agent, wanted_agents, opts);
        }
    }
    files
}

fn date_range_for_filter(since: SystemTime, until: Option<SystemTime>) -> Option<Vec<NaiveDate>> {
    let start = DateTime::<Local>::from(since).date_naive();
    let exclusive_end = until.unwrap_or_else(SystemTime::now);
    let end_adjusted = exclusive_end
        .checked_sub(Duration::from_millis(1))
        .unwrap_or(exclusive_end);
    let end = DateTime::<Local>::from(end_adjusted).date_naive();
    if end < start {
        return Some(Vec::new());
    }
    let day_count = end.signed_duration_since(start).num_days();
    if day_count > 120 {
        return None;
    }
    Some(
        (0..=day_count)
            .filter_map(|offset| start.checked_add_days(chrono::Days::new(offset as u64)))
            .collect(),
    )
}

fn push_candidate_if_session_file(
    files: &mut Vec<CandidateFile>,
    path: PathBuf,
    agent: &str,
    wanted_agents: &BTreeSet<String>,
    opts: &LiveGrepOptions,
) {
    if !wanted_agents.is_empty() && !wanted_agents.contains(agent) {
        return;
    }
    if !is_session_file(&path) {
        return;
    }
    let metadata = match path.metadata() {
        Ok(metadata) => metadata,
        Err(_) => return,
    };
    if !metadata.is_file() {
        return;
    }
    let modified = metadata.modified().ok();
    if !time_in_range(modified, opts.since, opts.until) {
        return;
    }
    files.push(CandidateFile {
        path,
        agent: agent.to_string(),
        modified,
        size_bytes: metadata.len(),
    });
}

fn is_hidden_noise_dir(entry: &DirEntry) -> bool {
    if !entry.file_type().is_dir() {
        return false;
    }
    let name = entry.file_name().to_string_lossy();
    matches!(
        name.as_ref(),
        ".git" | "target" | "node_modules" | ".cache" | ".tmp" | "tmp"
    )
}

fn is_session_file(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "jsonl" | "json" | "claude" | "md" | "txt"
    )
}

fn default_session_roots() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    vec![
        home.join(".codex/tabs"),
        home.join(".codex/sessions"),
        home.join(".claude/projects"),
        home.join(".pi/agent/sessions"),
        home.join(".gemini"),
        home.join(".cursor"),
        home.join("Library/Application Support/Cursor/User/workspaceStorage"),
    ]
}

fn infer_agent(path: &Path) -> String {
    let text = path.to_string_lossy().to_ascii_lowercase();
    if text.contains("/.codex/") {
        "codex".to_string()
    } else if text.contains("/.claude/") {
        "claude".to_string()
    } else if text.contains("/.pi/") {
        "pi_agent".to_string()
    } else if text.contains("/.gemini/") {
        "gemini".to_string()
    } else if text.contains("/cursor/") || text.contains("/.cursor/") {
        "cursor".to_string()
    } else {
        "unknown".to_string()
    }
}

fn time_in_range(
    modified: Option<SystemTime>,
    since: Option<SystemTime>,
    until: Option<SystemTime>,
) -> bool {
    let Some(modified) = modified else {
        return since.is_none() && until.is_none();
    };
    if let Some(since) = since
        && modified < since
    {
        return false;
    }
    if let Some(until) = until
        && modified >= until
    {
        return false;
    }
    true
}

enum Matcher {
    Literal {
        needle: String,
        normalized_needle: String,
        ignore_case: bool,
    },
    Regex(regex::Regex),
}

impl Matcher {
    fn new(query: &str, ignore_case: bool, regex: bool) -> Result<Self> {
        if regex {
            return RegexBuilder::new(query)
                .case_insensitive(ignore_case)
                .build()
                .map(Self::Regex)
                .with_context(|| "invalid live grep regex");
        }
        let needle = if ignore_case {
            query.to_ascii_lowercase()
        } else {
            query.to_string()
        };
        let normalized_needle = normalize_match_text(&needle);
        Ok(Self::Literal {
            needle,
            normalized_needle,
            ignore_case,
        })
    }

    fn matches(&self, haystack: &str) -> bool {
        match self {
            Self::Regex(regex) => regex.is_match(haystack),
            Self::Literal {
                needle,
                normalized_needle,
                ignore_case,
            } => {
                let candidate = if *ignore_case {
                    haystack.to_ascii_lowercase()
                } else {
                    haystack.to_string()
                };
                candidate.contains(needle)
                    || normalize_match_text(&candidate).contains(normalized_needle)
            }
        }
    }
}

fn normalize_match_text(text: &str) -> String {
    let text = text.replace("\\n", " ").replace("\\t", " ");
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn trim_snippet(line: &str) -> String {
    let clean = normalize_match_text(line);
    if clean.chars().count() <= MAX_SNIPPET_CHARS {
        return clean;
    }
    let mut snippet = clean.chars().take(MAX_SNIPPET_CHARS).collect::<String>();
    snippet.push_str("...");
    snippet
}

fn format_system_time(time: SystemTime) -> String {
    DateTime::<Utc>::from(time).to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn literal_match_normalizes_pasted_newlines() {
        let matcher = Matcher::new(
            "remote-preservation score was too generous because it had receipt only",
            true,
            false,
        )
        .expect("matcher");

        assert!(matcher.matches(
            "remote-preservation score was too generous because it had receipt\\n  only churn"
        ));
    }

    #[test]
    fn live_grep_scans_newest_session_files_with_bounds() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = tmp.path().join("rollout-1.jsonl");
        let mut file = File::create(&session).expect("create session");
        writeln!(file, "{{\"message\":\"needle in session\"}}").expect("write session");

        let opts = LiveGrepOptions {
            query: "needle".to_string(),
            roots: vec![tmp.path().to_path_buf()],
            agents: Vec::new(),
            limit: 5,
            max_files: 20,
            max_file_bytes: 1024 * 1024,
            max_hits_per_file: DEFAULT_MAX_HITS_PER_FILE,
            timeout: Duration::from_secs(1),
            include_compacted: false,
            since: None,
            until: None,
            time_filter_label: None,
            role: RoleFilter::Any,
            order: ScanOrder::Newest,
            ignore_case: true,
            regex: false,
        };

        let result = live_grep(&opts).expect("live grep");

        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.sessions.len(), 1);
        assert_eq!(result.sessions[0].hit_count, 1);
        assert_eq!(result.hits[0].line_number, 1);
        assert_eq!(result._meta.candidate_files, 1);
        assert_eq!(result._meta.scanned_files, 1);
        assert!(!result._meta.timed_out);
        assert_eq!(result._meta.intent, "direct_find");
        assert_eq!(
            result._meta.touched_subsystems,
            vec!["source_files".to_string()]
        );
        assert!(
            result
                ._meta
                .did_not_touch_subsystems
                .contains(&"sqlite".to_string())
        );
        assert!(
            result
                ._meta
                .did_not_touch_subsystems
                .contains(&"tantivy".to_string())
        );
        assert_eq!(result._meta.query_plan.engine, "live_grep");
        assert_eq!(result._meta.query_plan.intent, "direct_find");
        assert_eq!(
            result._meta.query_plan.reason,
            "source_log_first_exact_or_phrase_recovery"
        );
        assert_eq!(
            result._meta.query_plan.execution_path,
            "bounded-filesystem-scan"
        );
        assert_eq!(result._meta.query_plan.budget.threads, 1);
        assert_eq!(
            result._meta.query_plan.will_touch,
            result._meta.touched_subsystems
        );
        assert_eq!(
            result._meta.query_plan.will_not_touch,
            result._meta.did_not_touch_subsystems
        );
    }

    #[test]
    fn live_grep_caps_hits_per_session_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = tmp.path().join("rollout-many.jsonl");
        let mut file = File::create(&session).expect("create session");
        writeln!(file, "{{\"message\":\"needle one\"}}").expect("write one");
        writeln!(file, "{{\"message\":\"needle two\"}}").expect("write two");
        writeln!(file, "{{\"message\":\"needle three\"}}").expect("write three");

        let opts = LiveGrepOptions {
            query: "needle".to_string(),
            roots: vec![tmp.path().to_path_buf()],
            agents: Vec::new(),
            limit: 10,
            max_files: 20,
            max_file_bytes: 1024 * 1024,
            max_hits_per_file: 2,
            timeout: Duration::from_secs(1),
            include_compacted: false,
            since: None,
            until: None,
            time_filter_label: None,
            role: RoleFilter::Any,
            order: ScanOrder::Newest,
            ignore_case: true,
            regex: false,
        };

        let result = live_grep(&opts).expect("live grep");

        assert_eq!(result.hits.len(), 2);
        assert_eq!(result.sessions.len(), 1);
        assert_eq!(result.sessions[0].hit_count, 2);
    }

    #[test]
    fn session_summaries_preserve_first_hit_order() {
        let hits = vec![
            LiveGrepHit {
                source_path: "/tmp/b.jsonl".to_string(),
                line_number: 7,
                agent: "codex".to_string(),
                modified: None,
                snippet: "first b".to_string(),
            },
            LiveGrepHit {
                source_path: "/tmp/a.jsonl".to_string(),
                line_number: 3,
                agent: "codex".to_string(),
                modified: None,
                snippet: "first a".to_string(),
            },
            LiveGrepHit {
                source_path: "/tmp/b.jsonl".to_string(),
                line_number: 9,
                agent: "codex".to_string(),
                modified: None,
                snippet: "second b".to_string(),
            },
        ];

        let sessions = summarize_sessions(&hits);

        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].source_path, "/tmp/b.jsonl");
        assert_eq!(sessions[0].hit_count, 2);
        assert_eq!(sessions[1].source_path, "/tmp/a.jsonl");
    }

    #[test]
    fn candidate_sort_supports_oldest_first_retry() {
        let older = SystemTime::UNIX_EPOCH + Duration::from_secs(10);
        let newer = SystemTime::UNIX_EPOCH + Duration::from_secs(20);
        let mut candidates = vec![
            CandidateFile {
                path: PathBuf::from("/tmp/newer.jsonl"),
                agent: "codex".to_string(),
                modified: Some(newer),
                size_bytes: 1,
            },
            CandidateFile {
                path: PathBuf::from("/tmp/older.jsonl"),
                agent: "codex".to_string(),
                modified: Some(older),
                size_bytes: 1,
            },
        ];

        sort_candidates(&mut candidates, ScanOrder::Oldest);

        assert_eq!(candidates[0].path, PathBuf::from("/tmp/older.jsonl"));
    }

    #[test]
    fn role_filter_reduces_tool_output_noise() {
        assert!(line_matches_role(
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"cass search"}]}}"#,
            RoleFilter::User,
        ));
        assert!(!line_matches_role(
            r#"{"type":"response_item","payload":{"type":"function_call_output","output":"cass search"}}"#,
            RoleFilter::User,
        ));
        assert!(line_matches_role(
            r#"{"type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"cass search"}}"#,
            RoleFilter::Tool,
        ));
    }

    #[test]
    fn compacted_history_lines_are_detected() {
        assert!(is_compacted_history_line(
            r#"{"timestamp":"2026-05-19T11:44:31Z","type":"compacted","payload":{"replacement_history":[]}}"#
        ));
        assert!(!is_compacted_history_line(
            r#"{"timestamp":"2026-05-19T11:44:31Z","type":"response_item","payload":{}}"#
        ));
    }
}
