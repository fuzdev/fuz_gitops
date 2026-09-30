//! Shell text read as the commands it runs, for `hook`: a best-effort
//! reading of POSIX shell and bash syntax, never executed.
//!
//! `parse` splits a script into simple commands, each an argv with its
//! quoting removed, and gathers the scripts it holds — the bodies of
//! command substitutions (`$(…)`, backticks, `${…}`'s own), and of process
//! substitutions (`<(…)`, `>(…)`) — for the caller to read in turn.
//!
//! - **Words**: single quotes, double quotes (with their backslash rules),
//!   backslash escapes and line continuations, `$'…'` (ANSI-C escapes),
//!   `$"…"`, and brace expansion (`pu{sh,ll}`, letter sequences
//!   `{a..e..2}`, unquoted braces alone) as bash reads them, a word that
//!   expands to nothing dropped unless a quote opened in it (`{git,}
//!   push`, `x=; git $x push`) — past a budget on how much it reads, or at
//!   a group it can't read as bash would (a sequence through the
//!   non-letters between `Z` and `a`), a word it would expand is `DYN`.
//!   Number sequences (`{1..9}`) stay literal: their words differ only in
//!   digits and a sign, which spell no command or git subcommand.
//! - **Expansions**: `$NAME` and `${NAME}` take the value a plain
//!   assignment earlier in the same script gave (`g=git; $g push`, `export`,
//!   `declare`, `local`, `readonly`; `unset` forgets); any other expansion —
//!   an unknown variable, a positional or special parameter, `${…}` with an
//!   operator, a command or process substitution, arithmetic — is `DYN`, a
//!   marker the reader can't know.
//! - **Commands** end at `;` `&` `&&` `||` `|` `|&` a newline `(` `)`, and
//!   at an unquoted word that is exactly `{` or `}`; `NAME=value` words
//!   before the command name are its assignments. A `#` starting a word
//!   comments out the rest of its line. A pipe reaches the next command
//!   past newlines, and every command of a `(…)` or `{ …; }` group it
//!   feeds (`a | { b; c; }`).
//! - **Redirections** (`<` `>` `>>` `>|` `<>` `<&` `>&` `&>` `&>>`, an fd
//!   number before them) drop their target from the argv; a here-document
//!   (`<<`, `<<-`) and a here-string (`<<<`) become the command's stdin
//!   text, and an output redirection's target a file it writes. A
//!   here-document's delimiter is never expanded (`<<$x` ends at a `$x`
//!   line); quoted anywhere, its quotes are removed and its body isn't
//!   expanded.
//!
//! - **Arithmetic** — `$((…))`, a `((…))` command (`for ((…))` too), and
//!   `$[…]` — is read to its closing parens or bracket with no
//!   here-document or comment in it, and its text gathered as a script:
//!   bash runs `((cmd) )` as a subshell when it isn't arithmetic.
//!
//! Anything it can't delimit — an unterminated quote, substitution, or
//! `${`, or nesting past `MAX_NESTING` — is `Unreadable`. Not modelled:
//! aliases, functions, word splitting of an unquoted expansion's value
//! (`x='git push'; $x` reads as one word), arrays (`a=(…)` splits as a
//! subshell), `case` patterns beyond their `)`, and a `)` inside `$(…)`
//! that no `(` opened.

use std::collections::HashMap;

/// A part of a word the reader can't know: an expansion with no value
/// assigned in the script, or a command's output.
pub const DYN: char = '\0';

/// A command's words once the script's quoting and known expansions are
/// removed.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SimpleCommand {
    /// The `NAME=value` words before the command name, in order.
    pub assigns: Vec<(String, String)>,
    /// The argv, brace-expanded; empty for a command of assignments or
    /// redirections alone.
    pub words: Vec<String>,
    /// The text on its stdin that the script holds: here-document bodies
    /// and here-strings.
    pub stdin: Vec<String>,
    /// The files its output redirections (`>`, `>>`, `>|`, `&>`, `&>>`)
    /// write.
    pub writes: Vec<String>,
    /// Its stdin is the previous command's stdout (`|` or `|&` before it).
    pub piped: bool,
    /// It had a redirection (so a command of none but that still counts).
    redirected: bool,
}

impl SimpleCommand {
    /// Nothing read yet: no word, assignment, or redirection.
    const fn is_empty(&self) -> bool {
        self.words.is_empty() && self.assigns.is_empty() && !self.redirected
    }
}

/// A script read as commands.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Script {
    pub commands: Vec<SimpleCommand>,
    /// The scripts substitutions hold, which run wherever they appear.
    pub substitutions: Vec<String>,
    /// Some word's brace groups were left unexpanded, past a budget
    /// (`MAX_BRACE_CHARS`, `MAX_PARSE_BRACE_CHARS`) or at a group the
    /// reading can't know, or its expansion cut short (`MAX_BRACE_WORDS`):
    /// what it runs can't be known, not even its command names.
    pub unexpanded: bool,
}

/// Text the reader couldn't delimit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unreadable;

/// Reads `text` as a script.
///
/// # Errors
///
/// `Unreadable` for an unterminated quote, substitution, or `${`.
pub fn parse(text: &str) -> Result<Script, Unreadable> {
    Lexer::new(text, HashMap::new()).script()
}

/// The part of `word` after its last `/`.
pub fn basename(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

/// `word` as an assignment, `NAME=value` (or `NAME+=value`): its name and
/// value.
pub fn split_assignment(word: &str) -> Option<(&str, &str)> {
    let (name, value) = word.split_once('=')?;
    let name = name.strip_suffix('+').unwrap_or(name);
    is_name(name).then_some((name, value))
}

fn is_name(s: &str) -> bool {
    s.starts_with(|c: char| c == '_' || c.is_ascii_alphabetic())
        && s.chars().all(|c| c == '_' || c.is_ascii_alphanumeric())
}

/// Brace expansion yields at most this many words from one: past it, the
/// rest are dropped, and the word counts as unexpanded
/// (`Script::unexpanded`), so the hook denies it — a dropped word may be
/// the one that pushes (`git {-c,x.y=z,…,push}`).
const MAX_BRACE_WORDS: usize = 256;

/// Brace expansion reads at most this many chars for one word, over all
/// its expansions. Bounds the time a word of many groups takes.
const MAX_BRACE_CHARS: usize = 1 << 16;

/// Brace expansion reads at most this many chars for one text, over all
/// its words. Bounds the time a text of many words with groups takes.
///
/// Past either budget, a word whose groups are left unexpanded is `DYN`
/// whole, and the script says so (`Script::unexpanded`): its words can't
/// be known without the expansion, and literal braces would read as a word
/// bash never runs (`git pu{sh,ll}` past the budget must still read as a
/// push, and `{git,push}` as git pushing).
const MAX_PARSE_BRACE_CHARS: usize = 1 << 22;

/// Where a char of a word being read came from. A quote leaves a mark
/// where it opens, a char with no text, so the word reads as bash reads
/// its raw text: `{a.''.c}` holds no `..`, `{a..''c}` is no sequence, and
/// `""` is a word though it's empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Src {
    /// Unquoted: brace expansion and assignment only see these.
    Bare,
    Quoted,
    /// Quoted by a backslash outside quotes (`\,`).
    Escaped,
    /// Where a quote opened: none of the word's text.
    Mark,
}

/// A word being read: each char, and where it came from.
#[derive(Debug, Default, Clone)]
struct Word {
    chars: Vec<char>,
    src: Vec<Src>,
}

impl Word {
    fn push(&mut self, c: char, bare: bool) {
        self.chars.push(c);
        self.src.push(if bare { Src::Bare } else { Src::Quoted });
    }

    fn push_escaped(&mut self, c: char) {
        self.chars.push(c);
        self.src.push(Src::Escaped);
    }

    /// A quote opening here.
    fn mark(&mut self) {
        // a mark's char is never read
        self.chars.push('\'');
        self.src.push(Src::Mark);
    }

    fn push_quoted(&mut self, s: &str) {
        for c in s.chars() {
            self.push(c, false);
        }
    }

    fn text(&self) -> String {
        Made::default().with(&self.chars, &self.src).text
    }

    fn is_bare(&self, s: &str) -> bool {
        self.src.iter().all(|s| *s == Src::Bare) && self.chars.iter().copied().eq(s.chars())
    }

    /// As an assignment, when its name and `=` are unquoted.
    fn assignment(&self) -> Option<(String, String)> {
        let eq = self.chars.iter().position(|c| *c == '=')?;
        if !self.src[..=eq].iter().all(|s| *s == Src::Bare) {
            return None;
        }
        let text = self.text();
        let (name, value) = split_assignment(&text)?;
        Some((name.to_owned(), value.to_owned()))
    }

    /// The words brace expansion makes of it, reading from `budget`, what's
    /// left of the text's (`MAX_PARSE_BRACE_CHARS`); `unexpanded` is set
    /// when a group is left unexpanded past it. A word that expands to
    /// nothing is dropped, as bash drops it, unless a quote opened in it.
    fn expand_braces(&self, budget: &mut usize, unexpanded: &mut bool) -> Vec<String> {
        let mut out = Vec::new();
        let mut left = MAX_BRACE_CHARS.min(*budget);
        let start = left;
        expand_braces_into(
            &Made::default(),
            &self.chars,
            &self.src,
            &mut out,
            &mut left,
            unexpanded,
        );
        *budget -= start - left;
        out.into_iter()
            .filter(|made| made.quoted || !made.text.is_empty())
            .map(|made| made.text)
            .collect()
    }
}

/// A word as far as brace expansion has made it: its text, and whether a
/// quote opened in it.
#[derive(Debug, Default, Clone)]
struct Made {
    text: String,
    quoted: bool,
}

impl Made {
    /// This, then the text of `chars`.
    fn with(&self, chars: &[char], src: &[Src]) -> Self {
        let mut made = self.clone();
        for (c, s) in chars.iter().zip(src) {
            if *s == Src::Mark {
                made.quoted = true;
            } else {
                made.text.push(*c);
            }
        }
        made
    }

    /// This, then `next`.
    fn and(&self, next: &Self) -> Self {
        Self {
            text: format!("{}{}", self.text, next.text),
            quoted: self.quoted || next.quoted,
        }
    }

    fn unknown() -> Self {
        Self {
            text: DYN.to_string(),
            quoted: false,
        }
    }
}

/// Expands `chars` as bash does, after `done`, a start already expanded,
/// into `out`, up to `MAX_BRACE_WORDS` and the chars `budget` has left:
/// past the words, the rest are dropped, and past the chars, or at a group
/// bash may read two ways, a word with a group left is `DYN` — either way,
/// `unexpanded` set.
///
/// Bash's reading: its first group (`brace_group`) splits a word into a
/// start, the words the group makes, and a rest; each word it makes
/// follows the start, and the rest is expanded after each, never
/// the start again.
fn expand_braces_into(
    done: &Made,
    chars: &[char],
    src: &[Src],
    out: &mut Vec<Made>,
    budget: &mut usize,
    unexpanded: &mut bool,
) {
    // a word past the cap is dropped: what it ran can't be known
    if out.len() >= MAX_BRACE_WORDS {
        *unexpanded = true;
        return;
    }
    let cost = done.text.len() + chars.len();
    let within = *budget >= cost;
    if within {
        *budget -= cost;
    }
    let Some(group) = brace_group(chars, src) else {
        out.push(done.with(chars, src));
        return;
    };
    let makes = if within {
        group.makes(chars, src)
    } else {
        Makes::Unknown
    };
    let start = done.with(&chars[..group.open], &src[..group.open]);
    let (rest, rest_src) = (&chars[group.close + 1..], &src[group.close + 1..]);
    match makes {
        Makes::Parts(parts) => {
            for (from, to) in parts {
                let mut made = Vec::new();
                let (part, part_src) = (&chars[from..to], &src[from..to]);
                expand_braces_into(
                    &Made::default(),
                    part,
                    part_src,
                    &mut made,
                    budget,
                    unexpanded,
                );
                for m in &made {
                    expand_braces_into(&start.and(m), rest, rest_src, out, budget, unexpanded);
                }
            }
        }
        Makes::Letters(letters) => {
            // each letter costs the group's text at least: fewer chars
            // than a comma group's make as many words
            let charge = letters.len() * (group.close + 1 - group.open);
            if *budget < charge {
                out.push(Made::unknown());
                *unexpanded = true;
                return;
            }
            *budget -= charge;
            for c in letters {
                let letter = Made {
                    text: c.to_string(),
                    quoted: false,
                };
                expand_braces_into(&start.and(&letter), rest, rest_src, out, budget, unexpanded);
            }
        }
        Makes::Itself => {
            let group_src = &src[group.open..=group.close];
            let start = start.with(&chars[group.open..=group.close], group_src);
            expand_braces_into(&start, rest, rest_src, out, budget, unexpanded);
        }
        // what it expands to can't be known
        Makes::Unknown => {
            out.push(Made::unknown());
            *unexpanded = true;
        }
    }
}

/// A brace group bash expands: its `{`, the commas at its own depth, and
/// its `}`.
#[derive(Debug)]
struct Group {
    open: usize,
    commas: Vec<usize>,
    close: usize,
}

/// What a brace group makes of a word.
#[derive(Debug)]
enum Makes {
    /// Each part's expansions, by the part's range of chars.
    Parts(Vec<(usize, usize)>),
    /// A letter sequence's letters.
    Letters(Vec<char>),
    /// The group itself, braces and all.
    Itself,
    /// Something the reading can't know.
    Unknown,
}

impl Group {
    fn makes(&self, chars: &[char], src: &[Src]) -> Makes {
        let (from, to) = (self.open + 1, self.close);
        if !self.commas.is_empty() {
            let mut bounds = vec![self.open];
            bounds.extend(&self.commas);
            bounds.push(self.close);
            return Makes::Parts(bounds.windows(2).map(|p| (p[0] + 1, p[1])).collect());
        }
        // a `..` group: with a comma anywhere inside, bash drops its braces
        // and expands the inside as one part (`{a..{b,c}}` is `a..b a..c`)
        // — a quote's comma too (unless a backslash in the quote is just
        // before it, which this reading doesn't keep: unknown), but not an
        // escaped one
        let inside = &chars[from..to];
        let comma = |s: Src| inside.iter().zip(&src[from..to]).any(|p| p == (&',', &s));
        if comma(Src::Bare) {
            return Makes::Parts(vec![(from, to)]);
        }
        if comma(Src::Quoted) {
            return Makes::Unknown;
        }
        if src[from..to].iter().any(|s| *s != Src::Bare) {
            return Makes::Itself;
        }
        match letter_sequence(inside) {
            // bash expands `{Z..a}` through `\` and a backtick, which it
            // then reads as quoting and a substitution
            Some(letters) if !letters.iter().all(char::is_ascii_alphabetic) => Makes::Unknown,
            Some(letters) => Makes::Letters(letters),
            None => Makes::Itself,
        }
    }
}

/// The first group bash expands in `chars`: the leftmost unquoted `{`
/// whose `}` holds, at its own depth, a comma, or a `..` not just before
/// the `}`. One pass, pairing braces on a stack.
fn brace_group(chars: &[char], src: &[Src]) -> Option<Group> {
    let bare = |i: usize, c: char| chars.get(i) == Some(&c) && src[i] == Src::Bare;
    // each open `{`, its commas, and whether it holds a `..`
    let mut open: Vec<(usize, Vec<usize>, bool)> = Vec::new();
    let mut first: Option<Group> = None;
    for (i, c) in chars.iter().enumerate() {
        if src[i] != Src::Bare {
            continue;
        }
        match c {
            '{' => open.push((i, Vec::new(), false)),
            ',' => {
                if let Some((_, commas, _)) = open.last_mut() {
                    commas.push(i);
                }
            }
            '.' if bare(i + 1, '.') && !bare(i + 2, '}') => {
                if let Some((_, _, dots)) = open.last_mut() {
                    *dots = true;
                }
            }
            '}' => {
                if let Some((at, commas, dots)) = open.pop()
                    && (dots || !commas.is_empty())
                    && first.as_ref().is_none_or(|f| at < f.open)
                {
                    first = Some(Group {
                        open: at,
                        commas,
                        close: i,
                    });
                }
            }
            _ => {}
        }
    }
    first
}

/// The letters of a sequence, `a..e` or `a..e..2`, as bash makes them:
/// one ASCII letter at each end, from the first toward the last by the
/// step (1 when none or 0), the last only when a step lands on it. `None`
/// for what bash leaves literal, and for a number sequence, left literal
/// here: its words differ only in digits and a sign, which spell no
/// command, git subcommand, or option the hook reads, and expanding one
/// would put an ordinary loop (`for i in {1..1000}`) past the word cap.
fn letter_sequence(inside: &[char]) -> Option<Vec<char>> {
    let text: String = inside.iter().collect();
    let (first, rest) = text.split_once("..")?;
    let mut first = first.chars();
    let a = first.next().filter(char::is_ascii_alphabetic)?;
    let mut rest = rest.chars();
    let b = rest.next().filter(char::is_ascii_alphabetic)?;
    if first.next().is_some() {
        return None;
    }
    let step: i64 = match rest.as_str() {
        "" => 1,
        // read as `strtoimax` reads it: blanks, a sign, digits
        more => more
            .strip_prefix("..")?
            .trim_start_matches([' ', '\t', '\n', '\x0b', '\x0c', '\r'])
            .parse()
            .ok()?,
    };
    // bash won't negate the least step toward a later letter
    if a < b && step == i64::MIN {
        return None;
    }
    let step = usize::try_from(step.unsigned_abs())
        .unwrap_or(usize::MAX)
        .max(1);
    let (a, b) = (u8::try_from(a).ok()?, u8::try_from(b).ok()?);
    let bytes: Vec<u8> = if a <= b {
        (a..=b).step_by(step).collect()
    } else {
        (b..=a).rev().step_by(step).collect()
    };
    Some(bytes.into_iter().map(char::from).collect())
}

/// What `balanced` reads the inside of: commands, where here-documents
/// and comments are, or arithmetic (and a `${…}`), where neither is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Inside {
    Commands,
    Arithmetic,
}

/// A here-document waiting for its body, which starts on the next line.
#[derive(Debug)]
struct Heredoc {
    delimiter: String,
    strip_tabs: bool,
    /// The delimiter was unquoted, so the body is expanded.
    expand: bool,
    /// The index its command will have in the script's commands.
    command: usize,
}

struct Lexer {
    chars: Vec<char>,
    pos: usize,
    /// Variables plain assignments gave, by name.
    vars: HashMap<String, String>,
    substitutions: Vec<String>,
    heredocs: Vec<Heredoc>,
    /// How many quotes, substitutions, and `${…}`s the cursor is inside,
    /// bounded by `MAX_NESTING`.
    nesting: u32,
    /// What's left of `MAX_PARSE_BRACE_CHARS`.
    brace_budget: usize,
    /// A word's groups were left unexpanded (`Script::unexpanded`).
    unexpanded: bool,
    /// The `(…)` and `{ …; }` groups the cursor is inside.
    groups: Vec<CommandGroup>,
}

/// A `(…)` subshell or `{ …; }` group being read.
#[derive(Debug, Clone, Copy)]
struct CommandGroup {
    /// Opened by `(` (else `{`).
    paren: bool,
    /// Its stdin is a pipe: every command in it reads that pipe.
    piped: bool,
}

/// How deeply quotes, substitutions, and `${…}`s may nest in one text
/// before it's `Unreadable`: far past any real command, it bounds the
/// reader's recursion.
pub const MAX_NESTING: u32 = 64;

type Read<T = ()> = Result<T, Unreadable>;

impl Lexer {
    fn new(text: &str, vars: HashMap<String, String>) -> Self {
        Self {
            chars: text.chars().collect(),
            pos: 0,
            vars,
            substitutions: Vec::new(),
            heredocs: Vec::new(),
            nesting: 0,
            brace_budget: MAX_PARSE_BRACE_CHARS,
            unexpanded: false,
            groups: Vec::new(),
        }
    }

    /// Runs `read` one level deeper, or refuses past `MAX_NESTING`.
    fn nested<T>(&mut self, read: impl FnOnce(&mut Self) -> Read<T>) -> Read<T> {
        if self.nesting >= MAX_NESTING {
            return Err(Unreadable);
        }
        self.nesting += 1;
        let out = read(self);
        self.nesting -= 1;
        out
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek_at(&self, ahead: usize) -> Option<char> {
        self.chars.get(self.pos + ahead).copied()
    }

    fn next(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += 1;
        Some(c)
    }

    fn starts_with(&self, s: &str) -> bool {
        s.chars()
            .enumerate()
            .all(|(i, c)| self.peek_at(i) == Some(c))
    }

    fn script(mut self) -> Read<Script> {
        let mut script = Script::default();
        let mut cur = SimpleCommand::default();
        loop {
            self.skip_blanks();
            let Some(c) = self.peek() else { break };
            match c {
                '\n' => {
                    self.pos += 1;
                    self.finish(&mut script, &mut cur, false);
                    self.read_heredocs(&mut script)?;
                }
                '#' => {
                    while self.peek().is_some_and(|c| c != '\n') {
                        self.pos += 1;
                    }
                }
                ';' => {
                    while matches!(self.peek(), Some(';' | '&')) {
                        self.pos += 1;
                    }
                    self.finish(&mut script, &mut cur, false);
                }
                '&' if self.peek_at(1) == Some('>') => self.redirect(&script, &mut cur)?,
                '&' => {
                    self.pos += if self.peek_at(1) == Some('&') { 2 } else { 1 };
                    self.finish(&mut script, &mut cur, false);
                }
                '|' if self.peek_at(1) == Some('|') => {
                    self.pos += 2;
                    self.finish(&mut script, &mut cur, false);
                }
                '|' => {
                    self.pos += if self.peek_at(1) == Some('&') { 2 } else { 1 };
                    self.finish(&mut script, &mut cur, true);
                }
                // an arithmetic command, `((…))` (`for ((…))` too): no
                // here-document in it; its text is read as a script, as
                // bash runs `((cmd) )`
                '(' if self.peek_at(1) == Some('(') => {
                    self.finish(&mut script, &mut cur, false);
                    self.pos += 1;
                    let body = self.balanced('(', ')', Inside::Arithmetic)?;
                    self.substitutions.push(body);
                }
                '(' => {
                    self.pos += 1;
                    self.finish(&mut script, &mut cur, false);
                    self.open_group(&cur, true);
                }
                ')' => {
                    self.pos += 1;
                    self.finish(&mut script, &mut cur, false);
                    self.close_group(true);
                }
                '<' | '>' if self.peek_at(1) == Some('(') => {
                    self.pos += 2;
                    let body = self.balanced_parens()?;
                    self.substitutions.push(body);
                    let mut w = Word::default();
                    w.push(DYN, false);
                    self.add_word(&mut cur, &w);
                }
                '<' | '>' => self.redirect(&script, &mut cur)?,
                c if c.is_ascii_digit() && self.fd_prefix_len().is_some() => {
                    self.pos += self.fd_prefix_len().unwrap_or(0);
                    self.redirect(&script, &mut cur)?;
                }
                _ => {
                    let w = self.word()?;
                    let (open, close) = (w.is_bare("{"), w.is_bare("}"));
                    if open || close {
                        // a reserved word only where a command starts
                        let starts = cur.is_empty();
                        self.finish(&mut script, &mut cur, false);
                        if starts && open {
                            self.open_group(&cur, false);
                        } else if starts {
                            self.close_group(false);
                        }
                    } else {
                        self.add_word(&mut cur, &w);
                    }
                }
            }
        }
        self.finish(&mut script, &mut cur, false);
        // a here-document the text ends before: its body is what's left
        self.read_heredocs(&mut script)?;
        script.substitutions = self.substitutions;
        script.unexpanded = self.unexpanded;
        Ok(script)
    }

    fn skip_blanks(&mut self) {
        loop {
            match self.peek() {
                Some(' ' | '\t') => self.pos += 1,
                Some('\\') if self.peek_at(1) == Some('\n') => self.pos += 2,
                _ => return,
            }
        }
    }

    /// The length of the digits at the cursor when a redirection operator
    /// follows them (`2>`), making them its fd.
    fn fd_prefix_len(&self) -> Option<usize> {
        let digits = self.chars[self.pos..]
            .iter()
            .take_while(|c| c.is_ascii_digit())
            .count();
        matches!(self.peek_at(digits), Some('<' | '>')).then_some(digits)
    }

    fn add_word(&mut self, cur: &mut SimpleCommand, w: &Word) {
        if cur.words.is_empty()
            && let Some(assign) = w.assignment()
        {
            cur.assigns.push(assign);
            return;
        }
        cur.words
            .extend(w.expand_braces(&mut self.brace_budget, &mut self.unexpanded));
    }

    /// Ends the command being read, if it has anything, and records the
    /// variables it assigns; the next one reads a pipe when `piped`, or
    /// when this one read one and had nothing (a newline or a group after
    /// the `|`).
    fn finish(&mut self, script: &mut Script, cur: &mut SimpleCommand, piped: bool) {
        if cur.is_empty() {
            cur.piped |= piped;
            return;
        }
        cur.piped |= self.groups.last().is_some_and(|g| g.piped);
        self.record_vars(cur);
        script.commands.push(std::mem::take(cur));
        cur.piped = piped;
    }

    /// Opens a group at the command `cur` starts: piped when `cur` reads a
    /// pipe or the group it's in does.
    fn open_group(&mut self, cur: &SimpleCommand, paren: bool) {
        let piped = cur.piped || self.groups.last().is_some_and(|g| g.piped);
        self.groups.push(CommandGroup { paren, piped });
    }

    /// Closes the innermost group, when a `)` (`paren`) or `}` closes it.
    fn close_group(&mut self, paren: bool) {
        if self.groups.last().is_some_and(|g| g.paren == paren) {
            self.groups.pop();
        }
    }

    fn record_vars(&mut self, cur: &SimpleCommand) {
        let Some(name) = cur.words.first() else {
            for (n, v) in &cur.assigns {
                self.vars.insert(n.clone(), v.clone());
            }
            return;
        };
        match name.as_str() {
            "export" | "declare" | "typeset" | "local" | "readonly" => {
                for w in &cur.words[1..] {
                    if let Some((n, v)) = split_assignment(w) {
                        self.vars.insert(n.to_owned(), v.to_owned());
                    }
                }
            }
            "unset" => {
                for w in &cur.words[1..] {
                    self.vars.remove(w);
                }
            }
            _ => {}
        }
    }

    /// A redirection at the cursor, its fd already passed.
    fn redirect(&mut self, script: &Script, cur: &mut SimpleCommand) -> Read {
        let ops = [
            "&>>", "&>", "<<<", "<<-", "<<", "<>", "<&", "<", ">>", ">&", ">|", ">",
        ];
        let op = ops
            .into_iter()
            .find(|op| self.starts_with(op))
            .unwrap_or(">");
        self.pos += op.chars().count();
        cur.redirected = true;
        self.skip_blanks();
        if self.peek().is_none_or(|c| " \t\n;&|()<>".contains(c)) {
            return Ok(());
        }
        if matches!(op, "<<" | "<<-") {
            let (delimiter, quoted) = self.delimiter()?;
            self.heredocs.push(Heredoc {
                delimiter,
                strip_tabs: op == "<<-",
                expand: !quoted,
                command: script.commands.len(),
            });
            return Ok(());
        }
        let target = self.word()?;
        match op {
            "<<<" => cur.stdin.push(target.text()),
            ">" | ">>" | ">|" | "&>" | "&>>" => cur.writes.push(target.text()),
            _ => {}
        }
        Ok(())
    }

    /// Reads the bodies of the here-documents waiting, from the cursor:
    /// each up to its delimiter's line, or the end of the text.
    fn read_heredocs(&mut self, script: &mut Script) -> Read {
        for doc in std::mem::take(&mut self.heredocs) {
            let mut body = String::new();
            while self.pos < self.chars.len() {
                let end = self.chars[self.pos..]
                    .iter()
                    .position(|c| *c == '\n')
                    .map_or(self.chars.len(), |n| self.pos + n);
                let line: String = self.chars[self.pos..end].iter().collect();
                self.pos = (end + 1).min(self.chars.len());
                let line = if doc.strip_tabs {
                    line.trim_start_matches('\t')
                } else {
                    &line
                };
                if line == doc.delimiter {
                    break;
                }
                body.push_str(line);
                body.push('\n');
            }
            let body = if doc.expand {
                self.expand_text(&body)?
            } else {
                body
            };
            if let Some(command) = script.commands.get_mut(doc.command) {
                command.stdin.push(body);
            }
        }
        Ok(())
    }

    /// `text` expanded as a double-quoted string is, with no closing quote:
    /// a here-document body, or a `${…}`'s inside.
    fn expand_text(&mut self, text: &str) -> Read<String> {
        let mut sub = Self::new(text, self.vars.clone());
        // one level deeper, so `${a:-${a:-…}}` meets the bound in `balanced`
        sub.nesting = self.nesting + 1;
        let mut w = Word::default();
        sub.double_quoted(&mut w, None)?;
        self.substitutions.append(&mut sub.substitutions);
        Ok(w.text())
    }

    /// A word at the cursor, up to an unquoted blank or operator.
    fn word(&mut self) -> Read<Word> {
        let mut w = Word::default();
        while let Some(c) = self.peek() {
            match c {
                ' ' | '\t' | '\n' | ';' | '&' | '|' | '(' | ')' | '<' | '>' => break,
                '\'' => {
                    self.pos += 1;
                    w.mark();
                    loop {
                        match self.next().ok_or(Unreadable)? {
                            '\'' => break,
                            c => w.push(c, false),
                        }
                    }
                }
                '"' => {
                    self.pos += 1;
                    w.mark();
                    self.double_quoted(&mut w, Some('"'))?;
                }
                '\\' => {
                    self.pos += 1;
                    match self.next() {
                        Some('\n') => {}
                        Some(c) => w.push_escaped(c),
                        None => w.push('\\', false),
                    }
                }
                '$' => {
                    self.pos += 1;
                    self.dollar(&mut w, false)?;
                }
                '`' => {
                    self.pos += 1;
                    self.backtick(&mut w)?;
                }
                c => {
                    self.pos += 1;
                    w.push(c, true);
                }
            }
        }
        Ok(w)
    }

    /// A double-quoted string's inside, up to `end` (`None`: the end of the
    /// text).
    fn double_quoted(&mut self, w: &mut Word, end: Option<char>) -> Read {
        loop {
            let Some(c) = self.next() else {
                return if end.is_none() {
                    Ok(())
                } else {
                    Err(Unreadable)
                };
            };
            if Some(c) == end {
                return Ok(());
            }
            match c {
                '\\' => match self.peek() {
                    Some(e @ ('$' | '`' | '"' | '\\')) => {
                        self.pos += 1;
                        w.push(e, false);
                    }
                    Some('\n') => self.pos += 1,
                    _ => w.push('\\', false),
                },
                '$' => self.dollar(w, true)?,
                '`' => self.backtick(w)?,
                c => w.push(c, false),
            }
        }
    }

    /// An expansion after a `$`.
    fn dollar(&mut self, w: &mut Word, quoted: bool) -> Read {
        match self.peek() {
            Some('(') => {
                self.pos += 1;
                let body = self.balanced_parens()?;
                self.substitutions.push(body);
                w.push(DYN, false);
            }
            // `$[…]`, the old arithmetic
            Some('[') => {
                self.pos += 1;
                let body = self.balanced_brackets()?;
                self.substitutions.push(body);
                w.push(DYN, false);
            }
            Some('{') => {
                self.pos += 1;
                let inner = self.balanced_braces()?;
                if is_name(&inner) {
                    self.push_var(w, &inner);
                } else {
                    // `${x:-$(cmd)}` runs cmd
                    self.expand_text(&inner)?;
                    w.push(DYN, false);
                }
            }
            Some('\'') if !quoted => {
                self.pos += 1;
                w.mark();
                self.ansi_c(w)?;
            }
            Some('"') if !quoted => {
                self.pos += 1;
                w.mark();
                self.double_quoted(w, Some('"'))?;
            }
            Some(c) if c == '_' || c.is_ascii_alphabetic() => {
                let start = self.pos;
                while self
                    .peek()
                    .is_some_and(|c| c == '_' || c.is_ascii_alphanumeric())
                {
                    self.pos += 1;
                }
                let name: String = self.chars[start..self.pos].iter().collect();
                self.push_var(w, &name);
            }
            Some(c) if c.is_ascii_digit() || "@*#?$!-".contains(c) => {
                self.pos += 1;
                w.push(DYN, false);
            }
            _ => w.push('$', !quoted),
        }
        Ok(())
    }

    fn push_var(&self, w: &mut Word, name: &str) {
        match self.vars.get(name) {
            Some(value) => w.push_quoted(value),
            None => w.push(DYN, false),
        }
    }

    /// A backtick substitution's inside, up to its closing backtick.
    fn backtick(&mut self, w: &mut Word) -> Read {
        let mut body = String::new();
        loop {
            match self.next().ok_or(Unreadable)? {
                '`' => break,
                '\\' => match self.peek() {
                    Some(e @ ('`' | '\\' | '$')) => {
                        self.pos += 1;
                        body.push(e);
                    }
                    _ => body.push('\\'),
                },
                c => body.push(c),
            }
        }
        self.substitutions.push(body);
        w.push(DYN, false);
        Ok(())
    }

    /// `$'…'`'s inside, its escapes decoded.
    fn ansi_c(&mut self, w: &mut Word) -> Read {
        loop {
            let c = self.next().ok_or(Unreadable)?;
            if c == '\'' {
                return Ok(());
            }
            if c != '\\' {
                w.push(c, false);
                continue;
            }
            let e = self.next().ok_or(Unreadable)?;
            let decoded = match e {
                'n' => Some('\n'),
                't' => Some('\t'),
                'r' => Some('\r'),
                'a' => Some('\x07'),
                'b' => Some('\x08'),
                'e' | 'E' => Some('\x1b'),
                'f' => Some('\x0c'),
                'v' => Some('\x0b'),
                '\\' | '\'' | '"' | '?' => Some(e),
                'x' => self.code_point(16, 2),
                'u' => self.code_point(16, 4),
                'U' => self.code_point(16, 8),
                '0'..='7' => {
                    self.pos -= 1;
                    self.code_point(8, 3)
                }
                'c' => self
                    .next()
                    .and_then(|c| char::from_u32(u32::from(c) & 0x1f)),
                _ => None,
            };
            if let Some(c) = decoded {
                w.push(c, false);
            } else {
                w.push('\\', false);
                w.push(e, false);
            }
        }
    }

    /// Up to `max` digits in `radix` at the cursor, as a char.
    fn code_point(&mut self, radix: u32, max: usize) -> Option<char> {
        let mut value = 0u32;
        let mut n = 0;
        while n < max
            && let Some(d) = self.peek().and_then(|c| c.to_digit(radix))
        {
            value = value * radix + d;
            self.pos += 1;
            n += 1;
        }
        if n == 0 { None } else { char::from_u32(value) }
    }

    /// The text up to the `)` closing a `(` just passed, quotes and nested
    /// substitutions skipped.
    /// After `$(`: arithmetic when a second `(` follows (`$((…))`, its
    /// inner parens kept in the text), else a command substitution.
    fn balanced_parens(&mut self) -> Read<String> {
        if self.peek() == Some('(') {
            self.balanced('(', ')', Inside::Arithmetic)
        } else {
            self.balanced('(', ')', Inside::Commands)
        }
    }

    /// The text up to the `}` closing a `{` just passed.
    fn balanced_braces(&mut self) -> Read<String> {
        self.balanced('{', '}', Inside::Arithmetic)
    }

    /// The text up to the `]` closing a `$[` just passed.
    fn balanced_brackets(&mut self) -> Read<String> {
        self.balanced('[', ']', Inside::Arithmetic)
    }

    /// The text up to the `close` matching an `open` just passed. Quotes,
    /// substitutions, `${…}`, and `$[…]` are passed over, each read as its
    /// own unit, and, for commands, comments, arithmetic (`((…))`), and
    /// here-document bodies, so an apostrophe, `)`, or `}` in them doesn't
    /// count: `$(cat <<'EOF'` … `EOF` `)`, the commit-message idiom, reads
    /// whole, and so does `${x:-$(…)}` with a `}` in the substitution.
    fn balanced(&mut self, open: char, close: char, inside: Inside) -> Read<String> {
        self.nested(|lx| lx.balanced_inner(open, close, inside))
    }

    fn balanced_inner(&mut self, open: char, close: char, inside: Inside) -> Read<String> {
        let commands = inside == Inside::Commands;
        let start = self.pos;
        let mut depth = 1usize;
        // here-documents whose bodies start at the next newline
        let mut heredocs: Vec<(String, bool)> = Vec::new();
        loop {
            let at_word_start = self.pos == start
                || matches!(
                    self.chars[self.pos - 1],
                    ' ' | '\t' | '\n' | ';' | '&' | '|'
                );
            let c = self.next().ok_or(Unreadable)?;
            match c {
                '\\' => {
                    self.next();
                }
                '\'' => while self.next().ok_or(Unreadable)? != '\'' {},
                '"' => self.skip_double_quoted()?,
                '`' => self.skip_backtick()?,
                // a substitution or `${…}` is its own unit, read as its
                // kind is, whatever it's inside: a `}` or `)` in it (in a
                // here-document, a quote, or bare) never closes this
                '$' if self.peek() == Some('(') => {
                    self.pos += 1;
                    self.balanced_parens()?;
                }
                '$' if self.peek() == Some('{') => {
                    self.pos += 1;
                    self.balanced_braces()?;
                }
                '$' if self.peek() == Some('[') => {
                    self.pos += 1;
                    self.balanced_brackets()?;
                }
                // arithmetic, `((…))`: no here-document or comment in it
                '(' if commands && self.peek() == Some('(') => {
                    self.balanced('(', ')', Inside::Arithmetic)?;
                }
                // a comment in `$(…)`; `${#x}` is a length
                '#' if commands && at_word_start => {
                    while self.peek().is_some_and(|c| c != '\n') {
                        self.pos += 1;
                    }
                }
                '<' if commands && self.peek() == Some('<') => {
                    self.pos += 1;
                    match self.peek() {
                        // a here-string: its word is read as any other
                        Some('<') => self.pos += 1,
                        Some('-') => {
                            self.pos += 1;
                            heredocs.push((self.delimiter()?.0, true));
                        }
                        _ => heredocs.push((self.delimiter()?.0, false)),
                    }
                }
                '\n' => {
                    for (delimiter, strip_tabs) in std::mem::take(&mut heredocs) {
                        self.skip_heredoc_body(&delimiter, strip_tabs);
                    }
                }
                c if c == open => depth += 1,
                c if c == close => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(self.chars[start..self.pos - 1].iter().collect());
                    }
                }
                _ => {}
            }
        }
    }

    /// A here-document's delimiter word at the cursor, past blanks, as bash
    /// reads it: a word as any other is (substitutions and `${…}` its
    /// units), nothing in it expanded, and whether a quote or backslash
    /// outside those units quotes it — then the body isn't expanded, and
    /// its text is the word with quotes removed, inside the units too
    /// (`<<"$(a "b")"` ends at `$(a b)`); unquoted, it's the word as
    /// written (`<<$x` ends at a `$x` line).
    fn delimiter(&mut self) -> Read<(String, bool)> {
        while matches!(self.peek(), Some(' ' | '\t')) {
            self.pos += 1;
        }
        let start = self.pos;
        let mut quoted = false;
        while let Some(c) = self.peek() {
            self.pos += 1;
            match c {
                ' ' | '\t' | '\n' | ';' | '&' | '|' | '(' | ')' | '<' | '>' => {
                    self.pos -= 1;
                    break;
                }
                '\'' => {
                    quoted = true;
                    while self.next().ok_or(Unreadable)? != '\'' {}
                }
                '"' => {
                    quoted = true;
                    self.skip_double_quoted()?;
                }
                '\\' => {
                    quoted = true;
                    self.next();
                }
                '`' => self.skip_backtick()?,
                '$' => match self.peek() {
                    Some('\'') => {
                        quoted = true;
                        self.pos += 1;
                        self.ansi_c(&mut Word::default())?;
                    }
                    Some('"') => {
                        quoted = true;
                        self.pos += 1;
                        self.skip_double_quoted()?;
                    }
                    Some('(') => {
                        self.pos += 1;
                        self.balanced_parens()?;
                    }
                    Some('{') => {
                        self.pos += 1;
                        self.balanced_braces()?;
                    }
                    Some('[') => {
                        self.pos += 1;
                        self.balanced_brackets()?;
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        let word: String = self.chars[start..self.pos].iter().collect();
        if !quoted {
            return Ok((word, false));
        }
        Ok((Self::new(&word, HashMap::new()).quotes_removed()?, true))
    }

    /// The text with its quotes removed, as bash removes a quoted
    /// here-document delimiter's: char by char, never reading
    /// substitutions as units, `$'…'` decoded.
    fn quotes_removed(mut self) -> Read<String> {
        let mut text = String::new();
        let mut in_double = false;
        while let Some(c) = self.next() {
            match c {
                '\\' => match self.next() {
                    None => text.push('\\'),
                    Some(e) => {
                        if in_double && !matches!(e, '$' | '`' | '"' | '\\' | '\n') {
                            text.push('\\');
                        }
                        text.push(e);
                    }
                },
                '\'' if !in_double => loop {
                    match self.next() {
                        Some('\'') | None => break,
                        Some(q) => text.push(q),
                    }
                },
                '"' => in_double = !in_double,
                '$' if !in_double && self.peek() == Some('\'') => {
                    self.pos += 1;
                    let mut w = Word::default();
                    self.ansi_c(&mut w)?;
                    text.push_str(&w.text());
                }
                // `$"…"` is `"…"`
                '$' if !in_double && self.peek() == Some('"') => {}
                c => text.push(c),
            }
        }
        Ok(text)
    }

    /// Passes a here-document's body, the cursor at its first line: up to
    /// and including its delimiter's line, or to the end of the text.
    fn skip_heredoc_body(&mut self, delimiter: &str, strip_tabs: bool) {
        while self.pos < self.chars.len() {
            let end = self.chars[self.pos..]
                .iter()
                .position(|c| *c == '\n')
                .map_or(self.chars.len(), |n| self.pos + n);
            let line: String = self.chars[self.pos..end].iter().collect();
            self.pos = (end + 1).min(self.chars.len());
            let line = if strip_tabs {
                line.trim_start_matches('\t')
            } else {
                &line
            };
            if line == delimiter {
                return;
            }
        }
    }

    fn skip_double_quoted(&mut self) -> Read {
        self.nested(|lx| {
            loop {
                match lx.next().ok_or(Unreadable)? {
                    '\\' => {
                        lx.next();
                    }
                    '"' => return Ok(()),
                    '`' => lx.skip_backtick()?,
                    '$' if lx.peek() == Some('(') => {
                        lx.pos += 1;
                        lx.balanced_parens()?;
                    }
                    // quotes in it nest: `"${x:-"}"}"`
                    '$' if lx.peek() == Some('{') => {
                        lx.pos += 1;
                        lx.balanced_braces()?;
                    }
                    _ => {}
                }
            }
        })
    }

    fn skip_backtick(&mut self) -> Read {
        loop {
            match self.next().ok_or(Unreadable)? {
                '\\' => {
                    self.next();
                }
                '`' => return Ok(()),
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argvs(text: &str) -> Vec<Vec<String>> {
        parse(text)
            .unwrap()
            .commands
            .into_iter()
            .map(|c| c.words)
            .collect()
    }

    fn one(text: &str) -> Vec<String> {
        let mut all = argvs(text);
        assert_eq!(all.len(), 1, "{text}: {all:?}");
        all.remove(0)
    }

    #[test]
    fn quoting_is_removed() {
        assert_eq!(
            one(r#""git" 'push' g''it pu""sh \git"#),
            ["git", "push", "git", "push", "git"]
        );
        assert_eq!(
            one(r#"echo "a \"b\" \$c \\ \x""#),
            ["echo", r#"a "b" $c \ \x"#]
        );
        assert_eq!(
            one("echo $'\\x67it' $'p\\165sh' $'\\n'"),
            ["echo", "git", "push", "\n"]
        );
        assert_eq!(one("echo git\\\n push"), ["echo", "git", "push"]);
        assert_eq!(one(r#"echo $"x""#), ["echo", "x"]);
        assert_eq!(one("echo '' \"\""), ["echo", "", ""]);
    }

    #[test]
    fn commands_split_at_operators() {
        assert_eq!(
            argvs("a 1 && b; c || d | e |& f & g\nh (i) { j; } $(k)"),
            [
                vec!["a", "1"],
                vec!["b"],
                vec!["c"],
                vec!["d"],
                vec!["e"],
                vec!["f"],
                vec!["g"],
                vec!["h"],
                vec!["i"],
                vec!["j"],
                vec!["\0"],
            ]
        );
        let s = parse("a | b || c | d").unwrap();
        let piped: Vec<bool> = s.commands.iter().map(|c| c.piped).collect();
        assert_eq!(piped, [false, true, false, true]);
        // `{}` and `a{b}` are words
        assert_eq!(
            one("find -exec x {} a{b} ;"),
            ["find", "-exec", "x", "{}", "a{b}"]
        );
    }

    #[test]
    fn comments_run_to_the_end_of_the_line() {
        assert_eq!(argvs("a # b; c\nd e#f"), [vec!["a"], vec!["d", "e#f"]]);
    }

    #[test]
    fn assignments_come_before_the_command_name() {
        let s = parse("A=1 B+=2 'C=3' d E=4").unwrap();
        assert_eq!(
            s.commands[0].assigns,
            [("A".into(), "1".into()), ("B".into(), "2".into())]
        );
        assert_eq!(s.commands[0].words, ["C=3", "d", "E=4"]);
        let s = parse("X=; Y=\"\"").unwrap();
        assert_eq!(s.commands[0].assigns, [("X".into(), String::new())]);
        assert_eq!(s.commands[1].assigns, [("Y".into(), String::new())]);
    }

    #[test]
    fn known_variables_expand_and_the_rest_are_dynamic() {
        assert_eq!(argvs("g=git; $g push")[1], ["git", "push"]);
        assert_eq!(argvs("export g=git; ${g} push")[1], ["git", "push"]);
        assert_eq!(argvs("g=git; unset g; $g push")[2], ["\0", "push"]);
        assert_eq!(
            one("$HOME/bin/git $1 \"$@\" ${x%y} $(a) `b`"),
            ["\0/bin/git", "\0", "\0", "\0", "\0", "\0"]
        );
        // a lone `$` is literal
        assert_eq!(one("echo $ a$"), ["echo", "$", "a$"]);
    }

    #[test]
    fn substitutions_are_gathered() {
        let s = parse(r#"a $(b "$(c)") `d \`e\`` "$(f)" <(g) ${x:-$(h)} $((1+2))"#).unwrap();
        assert_eq!(
            s.substitutions,
            [r#"b "$(c)""#, "d `e`", "f", "g", "h", "(1+2)"]
        );
    }

    #[test]
    fn brace_expansion() {
        assert_eq!(one("git pu{sh,ll}"), ["git", "push", "pull"]);
        assert_eq!(one("a{b,c}{d,e}"), ["abd", "abe", "acd", "ace"]);
        assert_eq!(one("x{a,{b,c}}"), ["xa", "xb", "xc"]);
        assert_eq!(
            one("'{a,b}' \"{c,d}\" {e} {}"),
            ["{a,b}", "{c,d}", "{e}", "{}"]
        );
        let many = one(&"{a,b}".repeat(12));
        assert_eq!(many.len(), MAX_BRACE_WORDS);
        // cut short, it counts as unexpanded; exactly at the cap, it doesn't
        assert!(parse(&"{a,b}".repeat(12)).unwrap().unexpanded);
        let at_cap = parse(&"{a,b}".repeat(8)).unwrap();
        assert_eq!(at_cap.commands[0].words.len(), MAX_BRACE_WORDS);
        assert!(!at_cap.unexpanded);
        // bash's first group wins, and what it leaves before it is never
        // read again: `{x..c}` here isn't a sequence
        assert_eq!(one("{x.{.,y}c}"), ["{x..c}", "{x.yc}"]);
        assert_eq!(one("{{a..c},x}"), ["a", "b", "c", "x"]);
        assert_eq!(one("{a..c}{1,2}"), ["a1", "a2", "b1", "b2", "c1", "c2"]);
        assert_eq!(one("{a..b}{c..d}"), ["ac", "ad", "bc", "bd"]);
        assert_eq!(one("{a..c}} {{a..c}"), ["a}", "b}", "c}", "{a", "{b", "{c"]);
        // a `..` group that's no sequence stays whole, braces and all, and
        // the rest of the word is still expanded
        assert_eq!(one("{a..zz}{x,y}"), ["{a..zz}x", "{a..zz}y"]);
        assert_eq!(one("{x..{a..b}}"), ["{x..{a..b}}"]);
        assert_eq!(one("{{a..a}..c}"), ["{{a..a}..c}"]);
        // with a comma anywhere inside, bash drops a `..` group's braces
        assert_eq!(
            one("{a..c{x,y}} {a..{b,c}}"),
            ["a..cx", "a..cy", "a..b", "a..c"]
        );
        assert_eq!(one("{a..c,d}"), ["a..c", "d"]);
        // a `..` just before the `}` makes no group
        assert_eq!(
            one("{{a,b}..} {{a,b}...}"),
            ["{a..}", "{b..}", "a...", "b..."]
        );
        // a quoted comma there counts, an escaped one doesn't; one a
        // backslash in its quote may escape is unknown
        assert!(parse("echo {a..c','}").unwrap().unexpanded);
        assert!(parse("echo {a..c'\\,'}").unwrap().unexpanded);
        assert_eq!(one("{a..c\\,}"), ["{a..c,}"]);
        assert_eq!(one("{a..\\\\,c}"), ["a..\\", "c"]);
    }

    #[test]
    fn letter_sequences() {
        assert_eq!(one("git pus{h..h}"), ["git", "push"]);
        assert_eq!(one("{g..g}it gi{t..t}"), ["git", "git"]);
        assert_eq!(one("{a..e}"), ["a", "b", "c", "d", "e"]);
        assert_eq!(one("{e..a}"), ["e", "d", "c", "b", "a"]);
        assert_eq!(one("{a..z..5}"), ["a", "f", "k", "p", "u", "z"]);
        assert_eq!(one("{a..e..3} {z..a..12}"), ["a", "d", "z", "n", "b"]);
        // the step's sign never turns it around; 0 is 1; strtoimax's
        // blanks and `+` and leading zeros
        assert_eq!(one("{a..e..-2} {e..a..2}"), ["a", "c", "e", "e", "c", "a"]);
        assert_eq!(one("{a..c..0} {a..c..-0}"), ["a", "b", "c", "a", "b", "c"]);
        assert_eq!(
            one("{a..e..+2} {a..e..02} {a..e..\r2}"),
            ["a", "c", "e"].repeat(3)
        );
        assert_eq!(one("{a..e..9223372036854775807}"), ["a"]);
        assert_eq!(one("{e..a..-9223372036854775808}"), ["e"]);
        // what bash leaves literal
        for literal in [
            "{a..zz}",
            "{ab..c}",
            "{a..}",
            "{..a}",
            "{a...c}",
            "{-a..c}",
            "{a..1}",
            "{1..a}",
            "{a..e.}",
            "{a..e..}",
            "{a..e..x}",
            "{a..e..2x}",
            "{a..e..2.}",
            "{a..e..+-2}",
            "{a..c..2..3}",
            "{a..e..9223372036854775808}",
            "{a..e..-9223372036854775808}",
            "{\u{e9}..f}",
        ] {
            assert_eq!(one(literal), [literal], "{literal}");
        }
        // quoting anywhere inside leaves it literal, as bash reads the
        // quotes in the raw text
        assert_eq!(
            one("{a..'e'} {a..\\e} {a.''.c} {a..c\"\"}"),
            ["{a..e}", "{a..e}", "{a..c}", "{a..c}"]
        );
        // numbers stay literal
        assert_eq!(
            one("{1..3} {-1..2} x{a..b}{1..2}"),
            ["{1..3}", "{-1..2}", "xa{1..2}", "xb{1..2}"]
        );
        // through `[` `\` `]` `^` `_` and a backtick, bash's words are
        // quoting and substitutions: unknown
        for s in ["{Z..a}", "{a..Z}", "{Y..b..2}", "{A..z..10}"] {
            let script = parse(s).unwrap();
            assert!(script.unexpanded, "{s}");
            assert_eq!(script.commands[0].words, ["\0"], "{s}");
        }
        assert!(!parse("{A..Z}{a..z..4}").unwrap().unexpanded);
        // the word cap counts a sequence's words
        assert_eq!(one("{a..p}{a..p}").len(), MAX_BRACE_WORDS);
        assert!(!parse("{a..p}{a..p}").unwrap().unexpanded);
        assert!(parse("{a..z}{a..z}").unwrap().unexpanded);
        // each letter costs the group's text against the text's budget
        let letters = |n: usize| parse(&"{a..z} ".repeat(n)).unwrap().unexpanded;
        let words = MAX_PARSE_BRACE_CHARS / (26 * "{a..z}".len());
        assert!(!letters(words * 4 / 5));
        assert!(letters(words + 1));
    }

    #[test]
    fn a_word_that_expands_to_nothing_is_dropped() {
        assert_eq!(one("{git,} push"), ["git", "push"]);
        assert_eq!(one("a {,} b"), ["a", "b"]);
        assert_eq!(argvs("x=; git $x push ${x}")[1], ["git", "push"]);
        // unless a quote opened in it
        assert_eq!(one("'' \"\" $'' $\"\""), ["", "", "", ""]);
        assert_eq!(one("\"\"{,} {\"\",}"), ["", "", ""]);
        assert_eq!(argvs("x=; \"$x\" \"${x}\"")[1], ["", ""]);
        // a quote before an `=` makes no assignment
        assert_eq!(argvs("a''=b"), [vec!["a=b"]]);
    }

    #[test]
    fn redirections_leave_the_argv() {
        assert_eq!(
            one("a >out 2>&1 b <in c &>all >>app d 3<>rw"),
            ["a", "b", "c", "d"]
        );
    }

    #[test]
    fn a_pipe_reaches_past_newlines_and_into_groups() {
        let piped = |text: &str| -> Vec<bool> {
            parse(text)
                .unwrap()
                .commands
                .iter()
                .map(|c| c.piped)
                .collect()
        };
        assert_eq!(piped("a |\nb"), [false, true]);
        assert_eq!(piped("a | # c\nb"), [false, true]);
        assert_eq!(piped("a | (b)"), [false, true]);
        assert_eq!(piped("a | { b; }"), [false, true]);
        assert_eq!(piped("a | (b) | c"), [false, true, true]);
        // every command in a piped group reads the pipe
        assert_eq!(piped("a | { b; c; }"), [false, true, true]);
        assert_eq!(piped("a | (b; (c; d))"), [false, true, true, true]);
        assert_eq!(piped("a | { echo }; c; }"), [false, true, true]);
        // a `)` closes no `{` (a `case` pattern's)
        assert_eq!(
            piped("a | { case x in y) b;; esac; c; }"),
            [false, true, true, true, true]
        );
        // and none after it
        assert_eq!(piped("a | { b; }; c"), [false, true, false]);
        assert_eq!(piped("a | (b); c"), [false, true, false]);
        assert_eq!(piped("a; (b); { c; }"), [false, false, false]);
        assert_eq!(piped("a | b\nc"), [false, true, false]);
    }

    #[test]
    fn here_document_delimiters_are_never_expanded() {
        // unquoted: the delimiter as written, the body expanded
        let s = parse("cat <<$x\n$(y)\n$x\nz").unwrap();
        assert_eq!(s.commands[0].stdin, ["\0\n"]);
        assert_eq!(s.substitutions, ["y"]);
        assert_eq!(s.commands[1].words, ["z"]);
        let s = parse("cat <<$(x)\nhi\n$\n$(x)\nz").unwrap();
        assert_eq!(s.commands[0].stdin, ["hi\n$\n"]);
        assert!(s.substitutions.is_empty());
        assert_eq!(s.commands[1].words, ["z"]);
        for (text, delimiter) in [("${y}z", "${y}z"), ("`q`", "`q`"), ("$(x 'y')", "$(x 'y')")] {
            let s = parse(&format!("cat <<{delimiter}\nb\n{delimiter}\nz")).unwrap();
            assert_eq!(s.commands[0].stdin, ["b\n"], "{text}");
            assert_eq!(s.commands[1].words, ["z"], "{text}");
        }
        // quoted anywhere outside its substitutions: quotes removed, even
        // inside them, and the body as it is
        let s = parse("x=Q; cat <<\"$x\"\n$x\nz\nQ").unwrap();
        // the body ends at the `$x` line, as bash ends it
        assert_eq!(s.commands[1].stdin, [""]);
        assert_eq!(s.commands[2].words, ["z"]);
        assert_eq!(s.commands[3].words, ["Q"]);
        for (quoted, delimiter) in [
            ("a\\b", "ab"),
            ("\"a\\$b\"", "a$b"),
            ("\"a\\zb\"", "a\\zb"),
            ("'a'\"b\"c", "abc"),
            ("$'a\\x41'", "aA"),
            ("$\"Q\"", "Q"),
            ("\"$(x \"y\")\"", "$(x y)"),
        ] {
            let s = parse(&format!("cat <<{quoted}\n$(q)\n{delimiter}\nz")).unwrap();
            assert_eq!(s.commands[0].stdin, ["$(q)\n"], "{quoted}");
            assert!(s.substitutions.is_empty(), "{quoted}");
            assert_eq!(s.commands[1].words, ["z"], "{quoted}");
        }
    }

    #[test]
    fn here_documents_and_strings_are_stdin() {
        let s = parse("bash <<'EOF' | cat\ngit $x\nEOF\nnext").unwrap();
        assert_eq!(s.commands[0].stdin, ["git $x\n"]);
        assert_eq!(s.commands[2].words, ["next"]);
        let s = parse("g=git; bash <<EOF\n$g push $(y)\nEOF").unwrap();
        assert_eq!(s.commands[1].stdin, ["git push \0\n"]);
        assert_eq!(s.substitutions, ["y"]);
        // a delimiter quoted anywhere, even by an empty quote, is quoted
        let s = parse("cat <<''X\n$(y)\nX").unwrap();
        assert_eq!(s.commands[0].stdin, ["$(y)\n"]);
        assert!(s.substitutions.is_empty());
        let s = parse("cat <<-X\n\tbody\n\tX").unwrap();
        assert_eq!(s.commands[0].stdin, ["body\n"]);
        // unterminated: the rest of the text
        let s = parse("sh <<EOF\ngit push").unwrap();
        assert_eq!(s.commands[0].stdin, ["git push\n"]);
        let s = parse("sh <<< 'git push'").unwrap();
        assert_eq!(s.commands[0].stdin, ["git push"]);
    }

    #[test]
    fn nesting_is_bounded() {
        let quoted = |n: usize| format!("echo {}x{}", "\"$(".repeat(n), ")\"".repeat(n));
        assert!(parse(&quoted(10)).is_ok());
        assert_eq!(parse(&quoted(MAX_NESTING as usize)), Err(Unreadable));
        let defaults = |n: usize| format!("echo {}x{}", "${a:-".repeat(n), "}".repeat(n));
        assert!(parse(&defaults(10)).is_ok());
        assert_eq!(parse(&defaults(MAX_NESTING as usize + 1)), Err(Unreadable));
        // a word too long to expand can't be known; with no group, it's
        // itself
        let long = "x".repeat(MAX_BRACE_CHARS) + "{a,b}";
        assert_eq!(one(&long), ["\0"]);
        assert!(parse(&long).unwrap().unexpanded);
        let plain = "x".repeat(MAX_BRACE_CHARS) + "{ab}";
        assert!(!parse(&plain).unwrap().unexpanded);
        assert_eq!(one(&plain), [plain]);
        assert_eq!(one("x{a,b}").len(), 2);
        // bash expands `{push,x…}` to `push` first: never read as itself
        let first = format!("git {{push,{}}}", "x".repeat(MAX_BRACE_CHARS));
        assert_eq!(one(&first), ["git", "\0"]);
    }

    #[test]
    fn substitutions_pass_over_here_documents_and_comments() {
        let s = parse("echo \"$(cat <<'EOF'\ndon't ) `\nEOF\n)\" after").unwrap();
        assert_eq!(s.commands[0].words, ["echo", "\0", "after"]);
        assert_eq!(s.substitutions, ["cat <<'EOF'\ndon't ) `\nEOF\n"]);
        let s = parse("x=$(cat <<-\"E\"\n\tit's\n\tE\n); $(a # it's )\n)").unwrap();
        assert_eq!(
            s.substitutions,
            ["cat <<-\"E\"\n\tit's\n\tE\n", "a # it's )\n"]
        );
        // a here-string's word is read as any other
        assert!(parse("$(cat <<< \"it's\")").is_ok());
        let s = parse("echo ${#msg}; x=\"${#a}\" $(echo ${#b})").unwrap();
        assert_eq!(s.commands.len(), 2);
        assert_eq!(s.substitutions, ["echo ${#b}"]);
        let s = parse("$(cat <<< x\necho 'a'\n)").unwrap();
        assert_eq!(s.substitutions, ["cat <<< x\necho 'a'\n"]);
        assert_eq!(parse("$(cat <<< it's)"), Err(Unreadable));
    }

    /// Past the text's budget, a word with a group is `DYN`, however short;
    /// one with none is itself.
    #[test]
    fn brace_expansion_is_bounded_over_the_text() {
        let word = "{a,b}{c,d}{e,f}{g,h}{i,j}{k,l}{m,n}{o,p} ";
        let many = word.repeat(MAX_PARSE_BRACE_CHARS / 1000);
        let s = parse(&format!("echo {many}; git pu{{sh,ll}} x{{a}}")).unwrap();
        let echo = &s.commands[0].words;
        assert_eq!(echo[1], "acegikmo");
        assert_eq!(echo.last().map(String::as_str), Some("\0"));
        assert!(
            echo.len() < 256 * MAX_PARSE_BRACE_CHARS / 1000,
            "{}",
            echo.len()
        );
        assert_eq!(s.commands[1].words, ["git", "\0", "x{a}"]);
        assert!(s.unexpanded);
        // within it, as bash reads them
        let s = parse(&format!("{word}; git pu{{sh,ll}}")).unwrap();
        assert_eq!(s.commands[1].words, ["git", "push", "pull"]);
        assert!(!s.unexpanded);
    }

    #[test]
    fn nested_substitutions_are_units() {
        // a `}` in a substitution in `${…}` doesn't close it
        let s = parse("echo ${x:-$(cat <<'EOF'\n}\nEOF\n$g push\n)} after").unwrap();
        assert_eq!(s.commands[0].words, ["echo", "\0", "after"]);
        assert_eq!(s.substitutions, ["cat <<'EOF'\n}\nEOF\n$g push\n"]);
        let s = parse("echo ${x:-$(echo })}").unwrap();
        assert_eq!(s.substitutions, ["echo }"]);
        // nor does a `)` in a `${…}` in a substitution close that
        let s = parse("x=$(echo ${y:-)}) after").unwrap();
        assert_eq!(s.commands[0].words, ["after"]);
        assert_eq!(s.substitutions, ["echo ${y:-)}"]);
        // quotes nest in `${…}` in a double-quoted string in a substitution:
        // the `)` is quoted
        let s = parse("x=$(echo \"${y:-\")\"}\") after").unwrap();
        assert_eq!(s.commands[0].words, ["after"]);
        assert_eq!(s.substitutions, ["echo \"${y:-\")\"}\""]);
    }

    #[test]
    fn unterminated_text_is_unreadable() {
        for text in ["echo 'a", "echo \"a", "$(a", "`a", "${a", "$'a", "a <(b"] {
            assert_eq!(parse(text), Err(Unreadable), "{text}");
        }
    }
}
