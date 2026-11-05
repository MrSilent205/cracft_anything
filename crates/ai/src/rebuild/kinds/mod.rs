//! What is being made. Each kind of target (website, app, game) is a [`TargetKind`]: how a
//! request is recognised as one, its default stack, what the builder is told, and the
//! [`Check`]s that judge a build. A [`KindRegistry`] holds them; add a kind by implementing the
//! trait and calling [`KindRegistry::register`].

mod app;
mod game;
pub mod mechanics;
pub mod own;
pub mod plugin_check;
pub mod runs;
pub mod screenshot;
pub mod self_test;
pub mod website;

pub use app::{AppKind, RUNS_BRIEF};
pub use game::{CreditsCheck, GameKind};
pub use mechanics::MechanicsCheck;
pub use runs::RunsCheck;
pub use screenshot::ScreenshotCheck;
pub use self_test::SelfTestCheck;
pub use website::{WebVisualCheck, WebsiteKind};

use super::check::build_check;
use super::*;

/// What a kind is recognised from: the person's words, the target and the stack (when chosen).
pub struct DetectCx<'a> {
    /// Lower-case request prose padded with spaces, excluding URL tokens, for phrase matching.
    pub request: String,
    /// The same prose as whole lower-case words padded with spaces, punctuation removed.
    pub words: String,
    pub target: &'a TargetRef,
    /// The project's stack, or empty before one is chosen.
    pub stack: &'a str,
    /// Deep mode: the kind may look inside a local target (its files) to pick a stack.
    pub deep: bool,
}

impl<'a> DetectCx<'a> {
    /// Derive matching text from prose, excluding link destinations and standalone URL tokens.
    /// URL hosts, paths, queries and fragments must not select a project kind or named stack.
    /// This only changes detection text; the original request and explicit target stay intact.
    pub fn new(request: &str, target: &'a TargetRef, stack: &'a str) -> DetectCx<'a> {
        let prose = detection_prose(request);
        let words = stacks::words(&prose);
        let request = format!(" {prose} ");
        DetectCx { request, words, target, stack, deep: false }
    }

    /// Any of these phrases appears in the request.
    pub fn says(&self, phrases: &[&str]) -> bool {
        phrases.iter().any(|p| self.request.contains(p))
    }
}

/// Keep inline Markdown labels while skipping complete destinations, including relative links,
/// titles with spaces and balanced parentheses. Each iteration consumes request characters.
fn detection_prose(request: &str) -> String {
    let mut text = String::with_capacity(request.len());
    let mut chars = request.chars().peekable();
    let (mut brackets, mut destination) = (0usize, 0usize);
    while let Some(c) = chars.next() {
        if destination > 0 {
            match c {
                '\\' => {
                    chars.next();
                }
                '(' => destination = destination.saturating_add(1),
                ')' => destination -= 1,
                _ => {}
            }
        } else if c == '[' {
            brackets = brackets.saturating_add(1);
        } else if c == ']' && brackets > 0 {
            brackets -= 1;
            if chars.peek() == Some(&'(') {
                chars.next();
                destination = 1;
                text.push(' ');
            }
        } else {
            text.push(c);
        }
    }
    let lower = text.to_lowercase();
    lower.split_whitespace().map(token_prose).collect::<Vec<_>>().join(" ")
}

/// Preserve prose before an attached URL, such as `site:https://…` or `jogo(example.com/…)`.
fn token_prose(token: &str) -> &str {
    if url_token(token) {
        return "";
    }
    // Bound suffix comparisons on punctuation-heavy requests.
    const MAX_URL_PREFIXES: usize = 32;
    for (at, c) in token.char_indices().filter(|(_, c)| !c.is_alphanumeric() && !matches!(c, '-' | '_' | '.' | '+')).take(MAX_URL_PREFIXES) {
        let Some(suffix) = token.get(at..).and_then(|s| s.strip_prefix(c)) else { continue };
        if url_token(suffix) {
            return token.get(..at).unwrap_or_default();
        }
    }
    token
}

/// Recognize schemes, relative references and bare-domain addresses without interpreting their
/// host/path/query/fragment as prose. Dotted numeric versions and ordinary words are not domains.
fn url_token(token: &str) -> bool {
    let address = token.trim_matches(['(', ')', '<', '>', '"', '\'', '`', ',', ';', '.', '!', '…', '“', '”', '‘', '’', '{', '}']);
    let absolute = address.split_once("://").is_some_and(|(scheme, _)| {
        scheme.starts_with(|c: char| c.is_ascii_alphabetic()) && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    });
    // A general `word:` rule would consume prose such as `site:https://…` and `Next.js:/…`.
    let content_uri = address.split_once(':').is_some_and(|(scheme, rest)| !rest.is_empty() && matches!(scheme, "data" | "mailto"));
    if absolute || content_uri || address.starts_with(['/', '#', '?']) || address.starts_with("./") || address.starts_with("../") {
        return true;
    }
    let path = address.split(['?', '#']).next().unwrap_or_default();
    if let Some((directory, rest)) = path.split_once('/')
        && !directory.is_empty()
        && !rest.is_empty()
        && !directory.contains(':')
        && path.split('/').all(|part| !part.is_empty() && part.chars().all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '%' | '~')))
    {
        let words = stacks::words(path);
        return !stacks::catalogue().iter().any(|stack| stacks::words(&stack.name) == words);
    }
    let authority = address.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.parse::<std::net::IpAddr>().is_ok() {
        return true;
    }
    let host_port = authority.rsplit('@').next().unwrap_or_default();
    let host = if let Some((host, port)) = host_port.split_once(':') {
        let port = port.trim_end_matches(':');
        if port.is_empty() && !authority.contains('@') && catalog_stack_word(host) {
            return false;
        }
        if !port.is_empty() && (!port.bytes().all(|c| c.is_ascii_digit()) || port.parse::<u16>().is_err()) {
            return false;
        }
        host
    } else {
        host_port
    };
    if host.parse::<std::net::IpAddr>().is_ok() || (host == "localhost" && address != host) {
        return true;
    }
    if address == host && catalog_stack_word(host) {
        return false;
    }
    let Some((_, suffix)) = host.rsplit_once('.') else { return false };
    suffix.len() >= 2
        && suffix.chars().all(char::is_alphabetic)
        && host
            .split('.')
            .all(|label| !label.is_empty() && !label.starts_with('-') && !label.ends_with('-') && label.chars().all(|c| c.is_alphanumeric() || c == '-'))
}

/// Dotted names in the existing stack catalog remain prose, including before `:/relative-link`.
fn catalog_stack_word(word: &str) -> bool {
    stacks::catalogue().iter().any(|stack| stack.name.split_whitespace().any(|name| name.eq_ignore_ascii_case(word)))
}

/// What the brief is written from.
pub struct BriefCx<'a> {
    pub ws: &'a Workspace,
    /// Deep mode: decompiling the target is allowed (and required for programs).
    pub deep: bool,
    /// Ghidra is installed, so decompiling uses it.
    pub ghidra: bool,
    /// The game engine's editor for engine stacks.
    pub editor: Option<&'a Path>,
    /// An asset provider's key is set up; assets are offered only if the project also allows them
    /// (`Manifest::generate_assets`).
    pub assets: bool,
    /// The picture model chosen in Settings (an asset-table id); `None` = the table's default.
    pub image_model: Option<&'a str>,
}

/// The words the app uses for a kind's result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KindWords {
    pub button: &'static str,
    pub icon: &'static str,
    pub ready: &'static str,
    pub change_hint: &'static str,
}

/// A kind of thing Craft Anything can make.
pub trait TargetKind: Send + Sync {
    /// Stable id, stored with the project: `"website"`, `"app"`, `"game"`.
    fn id(&self) -> &'static str;
    /// How the app talks about the result.
    fn words(&self) -> KindWords;
    /// How strongly the request, target and stack say "this kind" (higher wins); `None` if not.
    fn detect(&self, cx: &DetectCx) -> Option<u32>;
    /// The stack used when the person names none.
    fn default_stack(&self, cx: &DetectCx) -> &'static str;
    /// A stack the request names that only this kind understands ("in Godot" for a game).
    fn named_stack(&self, _cx: &DetectCx) -> Option<&'static str> {
        None
    }
    /// For a folder target: also study the folder around the program (a game's content).
    fn studies_folder(&self) -> bool {
        false
    }
    /// "What to build" in the brief.
    fn goal(&self) -> &'static str;
    /// Extra brief sections after the shared steps and rules.
    fn brief_sections(&self, _cx: &BriefCx) -> Vec<String> {
        Vec::new()
    }
    /// Sections added to a follow-up message (a builder that resumes its session sees only that),
    /// for what may have changed since the brief: the project's settings as they are now.
    fn follow_up_sections(&self, _cx: &BriefCx) -> Vec<String> {
        Vec::new()
    }
    /// The result can't be photographed from outside, so the builder screenshots its own.
    fn self_screenshots(&self) -> bool {
        true
    }
    /// How a follow-up tells the builder to rebuild.
    fn rebuild_words(&self) -> &'static str {
        "build it again"
    }
    /// The checks that judge a build of this kind for this project, in order.
    fn checks(&self, ws: &Workspace) -> Vec<Box<dyn Check>>;
    /// The next round's message after a check that wasn't good enough.
    fn refine(&self, check: &CheckResult) -> String {
        format!(
            "Check {}: {}\n\nFix this in app/, build again until it succeeds, and reply with 1-2 plain sentences on what you changed.",
            check.round, check.note
        )
    }
}

/// What a check works with.
pub struct CheckCx<'a> {
    worker: &'a Worker,
    pub ws: &'a Workspace,
    pub round: u32,
}

impl CheckCx<'_> {
    /// Run an engine tool (the same ones the builder has).
    pub fn call(&self, tool: &str, params: Value) -> Result<ToolOutput, ToolError> {
        self.worker.call(tool, params)
    }

    /// Serve a folder on loopback as the run's preview; returns its address.
    pub fn serve_preview(&self, dir: &Path) -> Result<String, ToolError> {
        let server = serve_dir(dir).map_err(|e| ToolError::io(dir.display(), e))?;
        let url = server.url();
        *self.worker.preview.lock().unwrap_or_else(PoisonError::into_inner) = Some(server);
        self.worker.emit(RunEvent::Preview(url.clone()));
        Ok(url)
    }

    /// The game engine's editor for this run (engine stacks).
    pub fn editor(&self) -> Option<&Path> {
        self.worker.editor.as_deref()
    }

    /// Cancelled when the person stops the run.
    pub fn stop(&self) -> &CancelToken {
        &self.worker.stop
    }

    /// How builds are judged (`[build]` in settings).
    pub fn build_settings(&self) -> craft_anything_engine::settings::BuildSettings {
        self.worker.engine.session().settings.build.clone()
    }

    /// Deep mode is on (Ghidra enabled): the builder had to decompile a program target.
    pub fn deep(&self) -> bool {
        !self.worker.engine.session().clean_room()
    }

    /// The Godot installed on this computer, which opens a Godot project.
    pub fn godot(&self) -> Option<PathBuf> {
        let store = self.worker.engine.session().settings.engine_store_dir();
        use craft_anything_engines::game as engine_game;
        engine_game::inventory(&engine_game::Places::here(store)).best(engine_game::Builder::Godot).map(|i| i.program.clone())
    }
}

impl Worker {
    /// Run the checks the kind asks for in this round, in order, then the WASM plugins' checks for
    /// this kind. One that fails stops the rest: a later check needs what an earlier one proved (a
    /// screenshot needs a build).
    pub(super) fn run_checks(&self, ws: &Workspace, kind: &dyn TargetKind, round: u32) -> Result<Vec<CheckResult>, ToolError> {
        let mut cx = CheckCx { worker: self, ws, round };
        let mut out = Vec::new();
        let from_plugins = craft_anything_engine::wasm_plugins::checks(&self.engine.session());
        let plugin_checks = from_plugins
            .into_iter()
            .filter(|c| c.kinds.is_empty() || c.kinds.iter().any(|k| k == kind.id()))
            .map(|check| Box::new(plugin_check::PluginJudge { check, kind: kind.id() }) as Box<dyn Check>);
        let checks = if ws.manifest.own_project() { own::checks() } else { kind.checks(ws) };
        for c in checks.into_iter().chain(plugin_checks) {
            if !c.applies(&cx) {
                continue;
            }
            let r = c.run(&mut cx)?;
            let failed = !r.passed;
            out.push(r);
            if failed {
                break;
            }
        }
        Ok(out)
    }
}

/// One way of judging a build.
pub trait Check: Send + Sync {
    /// Stable id, stored in each [`CheckResult`].
    fn id(&self) -> &'static str;
    /// Whether it runs in this run at all (a skipped check leaves no result).
    fn applies(&self, _cx: &CheckCx) -> bool {
        true
    }
    fn run(&self, cx: &mut CheckCx) -> Result<CheckResult, ToolError>;
}

/// A check that runs only in Deep mode on a program target, where the brief asked for what it
/// checks (an app's mechanics sheet, the credits for a game's original files).
pub struct DeepOnly<C>(pub C);

impl<C: Check> Check for DeepOnly<C> {
    fn id(&self) -> &'static str {
        self.0.id()
    }
    fn applies(&self, cx: &CheckCx) -> bool {
        matches!(cx.ws.manifest.target, TargetRef::Local { .. }) && cx.deep() && self.0.applies(cx)
    }
    fn run(&self, cx: &mut CheckCx) -> Result<CheckResult, ToolError> {
        self.0.run(cx)
    }
}

/// The project's own build must succeed (cargo, npm, or the game engine's); a static site has none.
pub struct BuildCheck;

impl Check for BuildCheck {
    fn id(&self) -> &'static str {
        CheckResult::BUILD
    }
    fn run(&self, cx: &mut CheckCx) -> Result<CheckResult, ToolError> {
        let (passed, note) = build_check(&cx.ws.app_dir(), cx.ws.app_place(), &cx.ws.manifest.stack, cx.editor(), cx.stop());
        // #736: a failed build of an empty app/ says first that nothing was written.
        let note = if !passed && app_is_empty(cx.ws) { format!("{} {note}", no_files_yet(cx.ws)) } else { note };
        Ok(CheckResult::new(self.id(), cx.round, passed, note))
    }
}

/// The kinds a run can choose from.
#[derive(Clone)]
pub struct KindRegistry {
    kinds: Vec<Arc<dyn TargetKind>>,
}

impl std::fmt::Debug for KindRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.kinds.iter().map(|k| k.id())).finish()
    }
}

impl Default for KindRegistry {
    fn default() -> Self {
        KindRegistry::built_in()
    }
}

impl KindRegistry {
    /// Website, game and app.
    pub fn built_in() -> KindRegistry {
        KindRegistry { kinds: vec![Arc::new(WebsiteKind), Arc::new(GameKind), Arc::new(AppKind)] }
    }

    /// Add a kind, or replace the one with the same id.
    pub fn register(&mut self, kind: Arc<dyn TargetKind>) {
        self.kinds.retain(|k| k.id() != kind.id());
        self.kinds.push(kind);
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn TargetKind>> {
        self.kinds.iter().find(|k| k.id() == id).cloned()
    }

    pub fn ids(&self) -> Vec<&'static str> {
        self.kinds.iter().map(|k| k.id()).collect()
    }

    /// The kind that claims the project most strongly; ties go to the one registered first.
    pub fn detect(&self, request: &str, target: &TargetRef, stack: &str) -> Option<Arc<dyn TargetKind>> {
        let cx = DetectCx::new(request, target, stack);
        let mut best: Option<(u32, &Arc<dyn TargetKind>)> = None;
        for k in &self.kinds {
            if let Some(score) = k.detect(&cx)
                && best.is_none_or(|(b, _)| score > b)
            {
                best = Some((score, k));
            }
        }
        best.map(|(_, k)| k.clone())
    }

    /// The kind and stack for a new project: a stack the person names wins, else the kind's. In
    /// Deep mode (`deep`) the kind's default may come from looking inside the target.
    pub fn choose(&self, request: &str, target: &TargetRef, deep: bool) -> Option<(Arc<dyn TargetKind>, &'static str)> {
        let kind = self.detect(request, target, "")?;
        let cx = DetectCx { deep, ..DetectCx::new(request, target, "") };
        let stack = kind.named_stack(&cx).or_else(|| named_stack(&cx)).unwrap_or_else(|| kind.default_stack(&cx));
        Some((kind, stack))
    }

    /// The stack the request names, if it names one any kind knows ("in Godot", "use Electron").
    pub fn named(&self, request: &str, target: &TargetRef) -> Option<&'static str> {
        let cx = DetectCx::new(request, target, "");
        self.kinds.iter().find_map(|k| k.named_stack(&cx)).or_else(|| named_stack(&cx))
    }
}

/// Stacks any kind can be asked for by name (`asked_by` in `stacks.toml`).
fn named_stack(cx: &DetectCx) -> Option<&'static str> {
    stacks::asked_for(&cx.words).map(|s| s.name.as_str())
}
