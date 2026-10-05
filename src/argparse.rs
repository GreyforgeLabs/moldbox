//! A small, faithful re-implementation of the parts of Python 3.14's
//! `argparse` that the original CLI used: long-option abbreviation, attached
//! short-option values (`-mfull`), `--opt=value`, `--` handling,
//! negative-number positionals, sub-commands with aliases, and the exact
//! usage/error/help texts and exit codes (help/version exit 0, usage errors
//! exit 2).
//!
//! Arguments are processed as raw bytes so non-UTF-8 paths survive intact.

use crate::pyfmt::{display_bytes, parse_py_float, parse_py_int, py_repr};

/// Value conversion applied to an option argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ty {
    Str,
    Int,
    Float,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Help,
    Version,
    /// `store_true`.
    Flag,
    /// A single-argument option.
    Value {
        ty: Ty,
        choices: Option<&'static [&'static str]>,
    },
}

#[derive(Debug)]
pub struct Opt {
    pub strings: &'static [&'static str],
    pub dest: &'static str,
    pub kind: Kind,
}

#[derive(Debug)]
pub struct Pos {
    pub dest: &'static str,
    /// `nargs=PARSER` (the sub-command selector) instead of a single value.
    pub parser: Option<&'static [&'static str]>,
}

#[derive(Debug)]
pub struct Spec {
    pub prog: &'static str,
    pub usage: &'static str,
    pub help: &'static str,
    pub version: &'static str,
    pub opts: &'static [Opt],
    pub pos: &'static [Pos],
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bool(bool),
    Bytes(Vec<u8>),
    Int(i128),
    Float(f64),
}

/// Parsed values keyed by destination name.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Namespace {
    values: Vec<(&'static str, Value)>,
}

impl Namespace {
    pub fn set(&mut self, dest: &'static str, value: Value) {
        match self.values.iter_mut().find(|(d, _)| *d == dest) {
            Some(slot) => slot.1 = value,
            None => self.values.push((dest, value)),
        }
    }

    pub fn get(&self, dest: &str) -> Option<&Value> {
        self.values.iter().find(|(d, _)| *d == dest).map(|(_, v)| v)
    }

    pub fn flag(&self, dest: &str) -> bool {
        matches!(self.get(dest), Some(Value::Bool(true)))
    }

    pub fn bytes(&self, dest: &str) -> &[u8] {
        match self.get(dest) {
            Some(Value::Bytes(b)) => b,
            _ => b"",
        }
    }

    fn merge(&mut self, other: Namespace) {
        for (dest, value) in other.values {
            self.set(dest, value);
        }
    }
}

/// Early termination: text for stdout/stderr and the exit status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exit {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Spec {
    fn error(&self, message: &str) -> Exit {
        Exit {
            code: 2,
            stdout: String::new(),
            stderr: format!("{}{}: error: {}\n", self.usage, self.prog, message),
        }
    }

    fn action_error(&self, action: Action, message: &str) -> Exit {
        let name = match action {
            Action::Opt(i) => self.opts[i].strings.join("/"),
            Action::Pos(i) => self.pos[i].dest.to_string(),
        };
        self.error(&format!("argument {name}: {message}"))
    }

    fn lookup(&self, option: &[u8]) -> Option<usize> {
        self.opts
            .iter()
            .position(|o| o.strings.iter().any(|s| s.as_bytes() == option))
    }

    /// Option strings in registration order, with their action index.
    fn option_strings(&self) -> impl Iterator<Item = (&'static str, usize)> + '_ {
        self.opts
            .iter()
            .enumerate()
            .flat_map(|(i, o)| o.strings.iter().map(move |s| (*s, i)))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Opt(usize),
    Pos(usize),
}

#[derive(Debug, Clone)]
struct OptTuple {
    action: Option<usize>,
    option_string: Vec<u8>,
    sep: Option<Vec<u8>>,
    explicit: Option<Vec<u8>>,
}

fn partition_eq(arg: &[u8]) -> (&[u8], Option<&[u8]>) {
    match arg.iter().position(|b| *b == b'=') {
        Some(i) => (&arg[..i], Some(&arg[i + 1..])),
        None => (arg, None),
    }
}

fn looks_negative(arg: &[u8]) -> bool {
    // re.match(r'-\.?\d', arg)
    match arg {
        [b'-', b'.', d, ..] | [b'-', d, ..] => d.is_ascii_digit(),
        _ => false,
    }
}

impl Spec {
    fn get_option_tuples(&self, arg: &[u8]) -> Vec<OptTuple> {
        let mut result = Vec::new();
        let (prefix, explicit) = partition_eq(arg);
        let sep = explicit.map(|_| b"=".to_vec());
        let explicit = explicit.map(<[u8]>::to_vec);
        if arg.len() > 1 && arg[1] == b'-' {
            for (s, i) in self.option_strings() {
                if s.as_bytes().starts_with(prefix) {
                    result.push(OptTuple {
                        action: Some(i),
                        option_string: s.as_bytes().to_vec(),
                        sep: sep.clone(),
                        explicit: explicit.clone(),
                    });
                }
            }
        } else {
            let short_prefix = &arg[..arg.len().min(2)];
            let short_explicit = &arg[arg.len().min(2)..];
            for (s, i) in self.option_strings() {
                if s.as_bytes() == short_prefix {
                    result.push(OptTuple {
                        action: Some(i),
                        option_string: s.as_bytes().to_vec(),
                        sep: Some(Vec::new()),
                        explicit: Some(short_explicit.to_vec()),
                    });
                } else if s.as_bytes().starts_with(prefix) {
                    result.push(OptTuple {
                        action: Some(i),
                        option_string: s.as_bytes().to_vec(),
                        sep: sep.clone(),
                        explicit: explicit.clone(),
                    });
                }
            }
        }
        result
    }

    fn parse_optional(&self, arg: &[u8]) -> Option<Vec<OptTuple>> {
        if arg.first() != Some(&b'-') {
            return None;
        }
        if let Some(i) = self.lookup(arg) {
            return Some(vec![OptTuple {
                action: Some(i),
                option_string: arg.to_vec(),
                sep: None,
                explicit: None,
            }]);
        }
        if arg.len() == 1 {
            return None;
        }
        if let (option, Some(explicit)) = partition_eq(arg)
            && let Some(i) = self.lookup(option)
        {
            return Some(vec![OptTuple {
                action: Some(i),
                option_string: option.to_vec(),
                sep: Some(b"=".to_vec()),
                explicit: Some(explicit.to_vec()),
            }]);
        }
        let tuples = self.get_option_tuples(arg);
        if !tuples.is_empty() {
            return Some(tuples);
        }
        if looks_negative(arg) || arg.contains(&b' ') {
            return None;
        }
        Some(vec![OptTuple {
            action: None,
            option_string: arg.to_vec(),
            sep: None,
            explicit: None,
        }])
    }
}

/// Callback invoked for the `nargs=PARSER` positional: receives the selected
/// command name and the remaining arguments, returns the sub-namespace and
/// any unrecognised arguments.
pub type SubParse<'a> =
    dyn FnMut(&[u8], &[Vec<u8>]) -> Result<(Namespace, Vec<Vec<u8>>), Exit> + 'a;

struct State<'s, 'c, 'd> {
    spec: &'s Spec,
    args: &'s [Vec<u8>],
    pattern: Vec<u8>,
    tuples: Vec<Option<Vec<OptTuple>>>,
    ns: Namespace,
    seen: Vec<Action>,
    extras: Vec<Vec<u8>>,
    unrecognized: Vec<Vec<u8>>,
    positionals: Vec<usize>,
    sub: &'c mut SubParse<'d>,
}

impl State<'_, '_, '_> {
    fn take_action(&mut self, action: Action, values: Vec<Vec<u8>>) -> Result<(), Exit> {
        if !self.seen.contains(&action) {
            self.seen.push(action);
        }
        match action {
            Action::Opt(i) => {
                let opt = &self.spec.opts[i];
                match opt.kind {
                    Kind::Help => Err(Exit {
                        code: 0,
                        stdout: self.spec.help.to_string(),
                        stderr: String::new(),
                    }),
                    Kind::Version => Err(Exit {
                        code: 0,
                        stdout: format!("{}\n", self.spec.version),
                        stderr: String::new(),
                    }),
                    Kind::Flag => {
                        self.ns.set(opt.dest, Value::Bool(true));
                        Ok(())
                    }
                    Kind::Value { ty, choices } => {
                        let raw = &values[0];
                        let value = convert(ty, raw).ok_or_else(|| {
                            let ty_name = if ty == Ty::Int { "int" } else { "float" };
                            self.spec.action_error(
                                action,
                                &format!("invalid {ty_name} value: {}", py_repr(raw)),
                            )
                        })?;
                        if let Some(choices) = choices
                            && !choices.iter().any(|c| c.as_bytes() == raw.as_slice())
                        {
                            return Err(self
                                .spec
                                .action_error(action, &invalid_choice(raw, choices)));
                        }
                        self.ns.set(opt.dest, value);
                        Ok(())
                    }
                }
            }
            Action::Pos(i) => {
                let pos = &self.spec.pos[i];
                match pos.parser {
                    None => {
                        let value = values.into_iter().next().unwrap_or_default();
                        self.ns.set(pos.dest, Value::Bytes(value));
                        Ok(())
                    }
                    Some(choices) => {
                        let name = values[0].clone();
                        if !choices.iter().any(|c| c.as_bytes() == name.as_slice()) {
                            return Err(self
                                .spec
                                .action_error(action, &invalid_choice(&name, choices)));
                        }
                        self.ns.set(pos.dest, Value::Bytes(name.clone()));
                        let (sub_ns, sub_extras) = (self.sub)(&name, &values[1..])?;
                        self.ns.merge(sub_ns);
                        self.unrecognized.extend(sub_extras);
                        Ok(())
                    }
                }
            }
        }
    }

    /// `_match_argument` for an optional: number of following args consumed.
    fn match_option_args(&self, opt: usize, pattern: &[u8]) -> Result<usize, Exit> {
        match self.spec.opts[opt].kind {
            Kind::Value { .. } => {
                if pattern.first() == Some(&b'A') {
                    Ok(1)
                } else {
                    Err(self
                        .spec
                        .action_error(Action::Opt(opt), "expected one argument"))
                }
            }
            _ => Ok(0),
        }
    }

    fn consume_optional(&mut self, start: usize) -> Result<usize, Exit> {
        let tuples = self.tuples[start].clone().expect("option index");
        if tuples.len() > 1 {
            let matches: Vec<String> = tuples
                .iter()
                .map(|t| display_bytes(&t.option_string))
                .collect();
            return Err(self.spec.error(&format!(
                "ambiguous option: {} could match {}",
                display_bytes(&self.args[start]),
                matches.join(", ")
            )));
        }
        let OptTuple {
            mut action,
            mut option_string,
            mut sep,
            mut explicit,
        } = tuples.into_iter().next().expect("one tuple");
        let mut action_tuples: Vec<(usize, Vec<Vec<u8>>)> = Vec::new();
        let stop;
        loop {
            let Some(act) = action else {
                self.extras.push(self.args[start].clone());
                return Ok(start + 1);
            };
            if let Some(exp) = explicit.clone() {
                let count = match self.spec.opts[act].kind {
                    Kind::Value { .. } => 1,
                    _ => 0,
                };
                if count == 0 && option_string.get(1) != Some(&b'-') && !exp.is_empty() {
                    let has_sep = sep.as_ref().is_some_and(|s| !s.is_empty());
                    if has_sep || exp[0] == b'-' {
                        return Err(self.spec.action_error(
                            Action::Opt(act),
                            &format!("ignored explicit argument {}", py_repr(&exp)),
                        ));
                    }
                    action_tuples.push((act, Vec::new()));
                    option_string = vec![b'-', exp[0]];
                    if let Some(next) = self.spec.lookup(&option_string) {
                        action = Some(next);
                        let rest = exp[1..].to_vec();
                        if rest.is_empty() {
                            sep = None;
                            explicit = None;
                        } else if rest[0] == b'=' {
                            sep = Some(b"=".to_vec());
                            explicit = Some(rest[1..].to_vec());
                        } else {
                            sep = Some(Vec::new());
                            explicit = Some(rest);
                        }
                    } else {
                        let mut extra = vec![b'-'];
                        extra.extend_from_slice(&exp);
                        self.extras.push(extra);
                        stop = start + 1;
                        break;
                    }
                } else if count == 1 {
                    stop = start + 1;
                    action_tuples.push((act, vec![exp]));
                    break;
                } else {
                    return Err(self.spec.action_error(
                        Action::Opt(act),
                        &format!("ignored explicit argument {}", py_repr(&exp)),
                    ));
                }
            } else {
                let begin = start + 1;
                let count = self.match_option_args(act, &self.pattern[begin..])?;
                stop = begin + count;
                action_tuples.push((act, self.args[begin..stop].to_vec()));
                break;
            }
        }
        for (act, values) in action_tuples {
            self.take_action(Action::Opt(act), values)?;
        }
        Ok(stop)
    }

    /// `_match_arguments_partial` for single-value and PARSER positionals.
    fn match_positionals(&self, pattern: &[u8]) -> Vec<usize> {
        for n in (1..=self.positionals.len()).rev() {
            let mut counts = Vec::with_capacity(n);
            let mut idx = 0;
            let mut ok = true;
            for &p in &self.positionals[..n] {
                let begin = idx;
                while pattern.get(idx) == Some(&b'-') {
                    idx += 1;
                }
                if pattern.get(idx) != Some(&b'A') {
                    ok = false;
                    break;
                }
                idx += 1;
                if self.spec.pos[p].parser.is_some() {
                    idx = pattern.len();
                } else {
                    while pattern.get(idx) == Some(&b'-') {
                        idx += 1;
                    }
                }
                counts.push(idx - begin);
            }
            if ok {
                return counts;
            }
        }
        Vec::new()
    }

    fn consume_positionals(&mut self, mut start: usize) -> Result<usize, Exit> {
        let counts = self.match_positionals(&self.pattern[start..]);
        let matched: Vec<usize> = self.positionals[..counts.len()].to_vec();
        for (p, count) in matched.into_iter().zip(counts.iter().copied()) {
            let mut values = self.args[start..start + count].to_vec();
            // PARSER strips a leading '--' only; single values strip the
            // first '--' anywhere in their slice.
            let has_marker = if self.spec.pos[p].parser.is_some() {
                self.pattern[start] == b'-'
            } else {
                self.pattern[start..start + count].contains(&b'-')
            };
            if has_marker && let Some(i) = values.iter().position(|v| v == b"--") {
                values.remove(i);
            }
            start += count;
            self.take_action(Action::Pos(p), values)?;
        }
        self.positionals.drain(..counts.len());
        Ok(start)
    }

    fn run(&mut self) -> Result<(), Exit> {
        let option_indices: Vec<usize> = (0..self.args.len())
            .filter(|i| self.tuples[*i].is_some())
            .collect();
        let max_option = option_indices.last().copied();
        let mut start = 0usize;
        while let Some(max) = max_option {
            if start > max {
                break;
            }
            let next_option = option_indices
                .iter()
                .copied()
                .find(|i| *i >= start)
                .unwrap_or(max + 1);
            if start != next_option {
                let end = self.consume_positionals(start)?;
                if end > start {
                    start = end;
                    continue;
                }
                start = end;
            }
            if self.tuples.get(start).and_then(Option::as_ref).is_none() {
                self.extras
                    .extend(self.args[start..next_option].iter().cloned());
                start = next_option;
            }
            start = self.consume_optional(start)?;
        }
        let stop = self.consume_positionals(start)?;
        self.extras.extend(self.args[stop..].iter().cloned());

        let missing: Vec<&str> = (0..self.spec.pos.len())
            .filter(|i| !self.seen.contains(&Action::Pos(*i)))
            .map(|i| self.spec.pos[i].dest)
            .collect();
        if !missing.is_empty() {
            return Err(self.spec.error(&format!(
                "the following arguments are required: {}",
                missing.join(", ")
            )));
        }
        Ok(())
    }
}

fn convert(ty: Ty, raw: &[u8]) -> Option<Value> {
    match ty {
        Ty::Str => Some(Value::Bytes(raw.to_vec())),
        Ty::Int => parse_py_int(raw).map(Value::Int),
        Ty::Float => parse_py_float(raw).map(Value::Float),
    }
}

fn invalid_choice(value: &[u8], choices: &[&str]) -> String {
    let listed: Vec<String> = choices.iter().map(|c| py_repr(c.as_bytes())).collect();
    format!(
        "invalid choice: {} (choose from {})",
        py_repr(value),
        listed.join(", ")
    )
}

/// `parse_known_args`: returns the namespace and unrecognised arguments
/// (own extras first, then those reported by a sub-command).
pub fn parse_known(
    spec: &Spec,
    args: &[Vec<u8>],
    defaults: Namespace,
    sub: &mut SubParse<'_>,
) -> Result<(Namespace, Vec<Vec<u8>>), Exit> {
    let mut pattern = Vec::with_capacity(args.len());
    let mut tuples = Vec::with_capacity(args.len());
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == b"--" {
            pattern.push(b'-');
            tuples.push(None);
            for _ in iter.by_ref() {
                pattern.push(b'A');
                tuples.push(None);
            }
            break;
        }
        match spec.parse_optional(arg) {
            Some(found) => {
                pattern.push(b'O');
                tuples.push(Some(found));
            }
            None => {
                pattern.push(b'A');
                tuples.push(None);
            }
        }
    }
    let mut state = State {
        spec,
        args,
        pattern,
        tuples,
        ns: defaults,
        seen: Vec::new(),
        extras: Vec::new(),
        unrecognized: Vec::new(),
        positionals: (0..spec.pos.len()).collect(),
        sub,
    };
    state.run()?;
    let mut extras = state.extras;
    extras.extend(state.unrecognized);
    Ok((state.ns, extras))
}

/// `parse_args`: like [`parse_known`] but unrecognised arguments are an error.
pub fn parse(
    spec: &Spec,
    args: &[Vec<u8>],
    defaults: Namespace,
    sub: &mut SubParse<'_>,
) -> Result<Namespace, Exit> {
    let (ns, extras) = parse_known(spec, args, defaults, sub)?;
    if !extras.is_empty() {
        let shown: Vec<String> = extras.iter().map(|e| display_bytes(e)).collect();
        return Err(spec.error(&format!("unrecognized arguments: {}", shown.join(" "))));
    }
    Ok(ns)
}
