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
//!   `$"…"`, and brace expansion (`pu{sh,ll}`, unquoted braces alone, not
//!   `{a..b}`) as bash reads them — past a budget on how much it reads, a
//!   word it would expand is `DYN`.
//! - **Expansions**: `$NAME` and `${NAME}` take the value a plain
//!   assignment earlier in the same script gave (`g=git; $g push`, `export`,
//!   `declare`, `local`, `readonly`; `unset` forgets); any other expansion —
//!   an unknown variable, a positional or special parameter, `${…}` with an
//!   operator, a command or process substitution, arithmetic — is `DYN`, a
//!   marker the reader can't know.
//! - **Commands** end at `;` `&` `&&` `||` `|` `|&` a newline `(` `)`, and
//!   at an unquoted word that is exactly `{` or `}`; `NAME=value` words
//!   before the command name are its assignments. A `#` starting a word
//!   comments out the rest of its line.
//! - **Redirections** (`<` `>` `>>` `>|` `<>` `<&` `>&` `&>` `&>>`, an fd
//!   number before them) drop their target from the argv; a here-document
//!   (`<<`, `<<-`, its body expanded when the delimiter is unquoted) and a
//!   here-string (`<<<`) become the command's stdin text, and an output
//!   redirection's target a file it writes.
//!
//! - **Arithmetic** — `$((…))`, a `((…))` command (`for ((…))` too), and
//!   `$[…]` — is read to its closing parens or bracket with no
//!   here-document or comment in it, and its text gathered as a script:
//!   bash runs `((cmd) )` as a subshell when it isn't arithmetic.
//!
//! Anything it can't delimit — an unterminated quote, substitution, or
//! `${`, or nesting past `MAX_NESTING` — is `Unreadable`. Not modelled:
//! aliases, functions, `{a..b}`, arrays (`a=(…)` splits as a subshell),
//! `case` patterns beyond their `)`, and a `)` inside `$(…)` that no `(`
//! opened.

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

/// A script read as commands.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Script {
    pub commands: Vec<SimpleCommand>,
    /// The scripts substitutions hold, which run wherever they appear.
    pub substitutions: Vec<String>,
    /// Some word's brace groups were left unexpanded, past a budget
    /// (`MAX_BRACE_CHARS`, `MAX_PARSE_BRACE_CHARS`), or its expansion cut
    /// short (`MAX_BRACE_WORDS`): what it runs can't be known, not even its
    /// command names.
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

/// A word being read: each char, and whether it was unquoted (brace
/// expansion and assignment only see unquoted syntax).
#[derive(Debug, Default, Clone)]
struct Word {
    chars: Vec<char>,
    bare: Vec<bool>,
}

impl Word {
    fn push(&mut self, c: char, bare: bool) {
        self.chars.push(c);
        self.bare.push(bare);
    }

    fn push_quoted(&mut self, s: &str) {
        for c in s.chars() {
            self.push(c, false);
        }
    }

    fn text(&self) -> String {
        self.chars.iter().collect()
    }

    fn is_bare(&self, s: &str) -> bool {
        self.bare.iter().all(|b| *b) && self.chars.iter().copied().eq(s.chars())
    }

    /// As an assignment, when its name and `=` are unquoted.
    fn assignment(&self) -> Option<(String, String)> {
        let eq = self.chars.iter().position(|c| *c == '=')?;
        if !self.bare[..=eq].iter().all(|b| *b) {
            return None;
        }
        let text = self.text();
        let (name, value) = split_assignment(&text)?;
        Some((name.to_owned(), value.to_owned()))
    }

    /// The words brace expansion makes of it, reading from `budget`, what's
    /// left of the text's (`MAX_PARSE_BRACE_CHARS`); `unexpanded` is set
    /// when a group is left unexpanded past it.
    fn expand_braces(&self, budget: &mut usize, unexpanded: &mut bool) -> Vec<String> {
        let mut out = Vec::new();
        let mut left = MAX_BRACE_CHARS.min(*budget);
        let start = left;
        expand_braces_into(&self.chars, &self.bare, &mut out, &mut left, unexpanded);
        *budget -= start - left;
        out
    }
}

/// Expands the first unquoted `{…,…}` group of `chars`, then each result's
/// next, into `out`, up to `MAX_BRACE_WORDS` and the chars `budget` has
/// left: past the words, the rest are dropped, and past the chars, a word
/// with a group left is `DYN` — either way, `unexpanded` set.
fn expand_braces_into(
    chars: &[char],
    bare: &[bool],
    out: &mut Vec<String>,
    budget: &mut usize,
    unexpanded: &mut bool,
) {
    // a word past the cap is dropped: what it ran can't be known
    if out.len() >= MAX_BRACE_WORDS {
        *unexpanded = true;
        return;
    }
    let within = *budget >= chars.len();
    if within {
        *budget -= chars.len();
    }
    let Some((open, commas, close)) = brace_group(chars, bare) else {
        out.push(chars.iter().collect());
        return;
    };
    // out of budget with a group left: what it expands to can't be known
    if !within {
        out.push(DYN.to_string());
        *unexpanded = true;
        return;
    }
    let mut bounds = vec![open];
    bounds.extend(commas);
    bounds.push(close);
    for pair in bounds.windows(2) {
        let (start, end) = (pair[0] + 1, pair[1]);
        let mut c: Vec<char> = chars[..open].to_vec();
        let mut b: Vec<bool> = bare[..open].to_vec();
        c.extend_from_slice(&chars[start..end]);
        b.extend_from_slice(&bare[start..end]);
        c.extend_from_slice(&chars[close + 1..]);
        b.extend_from_slice(&bare[close + 1..]);
        expand_braces_into(&c, &b, out, budget, unexpanded);
    }
}

/// The first unquoted brace group with a comma at its own depth: its `{`,
/// its commas, and its `}`. One pass, pairing braces on a stack; of the
/// groups with a comma, the leftmost `{` wins, as bash expands them.
fn brace_group(chars: &[char], bare: &[bool]) -> Option<(usize, Vec<usize>, usize)> {
    let mut open: Vec<(usize, Vec<usize>)> = Vec::new();
    let mut first: Option<(usize, Vec<usize>, usize)> = None;
    for (i, c) in chars.iter().enumerate() {
        if !bare[i] {
            continue;
        }
        match c {
            '{' => open.push((i, Vec::new())),
            ',' => {
                if let Some((_, commas)) = open.last_mut() {
                    commas.push(i);
                }
            }
            '}' => {
                if let Some((at, commas)) = open.pop()
                    && !commas.is_empty()
                    && first.as_ref().is_none_or(|f| at < f.0)
                {
                    first = Some((at, commas, i));
                }
            }
            _ => {}
        }
    }
    first
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
                '(' | ')' => {
                    self.pos += 1;
                    self.finish(&mut script, &mut cur, false);
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
                    if w.is_bare("{") || w.is_bare("}") {
                        self.finish(&mut script, &mut cur, false);
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
    /// variables it assigns; the next one reads a pipe when `piped`.
    fn finish(&mut self, script: &mut Script, cur: &mut SimpleCommand, piped: bool) {
        if !cur.words.is_empty() || !cur.assigns.is_empty() || cur.redirected {
            self.record_vars(cur);
            script.commands.push(std::mem::take(cur));
        }
        cur.piped = piped;
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
        let target = self.word()?;
        match op {
            "<<<" => cur.stdin.push(target.text()),
            ">" | ">>" | ">|" | "&>" | "&>>" => cur.writes.push(target.text()),
            "<<" | "<<-" => self.heredocs.push(Heredoc {
                delimiter: target.text(),
                strip_tabs: op == "<<-",
                expand: target.bare.iter().all(|b| *b),
                command: script.commands.len(),
            }),
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
                    loop {
                        match self.next().ok_or(Unreadable)? {
                            '\'' => break,
                            c => w.push(c, false),
                        }
                    }
                }
                '"' => {
                    self.pos += 1;
                    self.double_quoted(&mut w, Some('"'))?;
                }
                '\\' => {
                    self.pos += 1;
                    match self.next() {
                        Some('\n') => {}
                        Some(c) => w.push(c, false),
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
                self.ansi_c(w)?;
            }
            Some('"') if !quoted => {
                self.pos += 1;
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
                            heredocs.push((self.delimiter(), true));
                        }
                        _ => heredocs.push((self.delimiter(), false)),
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

    /// A here-document's delimiter word at the cursor, past blanks, its
    /// quoting removed.
    fn delimiter(&mut self) -> String {
        while matches!(self.peek(), Some(' ' | '\t')) {
            self.pos += 1;
        }
        let mut delimiter = String::new();
        while let Some(c) = self.peek() {
            match c {
                ' ' | '\t' | '\n' | ';' | '&' | '|' | '(' | ')' | '<' | '>' => break,
                '\'' | '"' => {
                    self.pos += 1;
                    while let Some(q) = self.next() {
                        if q == c {
                            break;
                        }
                        delimiter.push(q);
                    }
                }
                '\\' => {
                    self.pos += 1;
                    delimiter.extend(self.next());
                }
                c => {
                    self.pos += 1;
                    delimiter.push(c);
                }
            }
        }
        delimiter
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
    }

    #[test]
    fn redirections_leave_the_argv() {
        assert_eq!(
            one("a >out 2>&1 b <in c &>all >>app d 3<>rw"),
            ["a", "b", "c", "d"]
        );
    }

    #[test]
    fn here_documents_and_strings_are_stdin() {
        let s = parse("bash <<'EOF' | cat\ngit $x\nEOF\nnext").unwrap();
        assert_eq!(s.commands[0].stdin, ["git $x\n"]);
        assert_eq!(s.commands[2].words, ["next"]);
        let s = parse("g=git; bash <<EOF\n$g push $(y)\nEOF").unwrap();
        assert_eq!(s.commands[1].stdin, ["git push \0\n"]);
        assert_eq!(s.substitutions, ["y"]);
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
