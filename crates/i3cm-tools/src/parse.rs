//! Option parsing, shared by the program and the server. Options and operands may be mixed as
//! with GNU; `--` ends the options. An option the tool does not know is always an error.

use crate::{Call, Exec, FindTest, Options, Request, Tool, ToolError, TOOLS};

/// The options a tool accepts, for help and error messages.
pub fn supported(tool: Tool) -> Vec<String> {
    let s: &[&str] = match tool {
        Tool::Ls => &["-l", "-a", "-1", "-R", "--all", "--recursive"],
        Tool::Cat | Tool::Stat => &[],
        Tool::Head => &["-n N", "-c N", "-N", "--lines=N", "--bytes=N"],
        Tool::Tail => &["-n [+]N", "-c [+]N", "-N", "--lines=[+]N", "--bytes=[+]N"],
        Tool::Wc => &["-l", "-w", "-c", "--lines", "--words", "--bytes"],
        Tool::Grep => &[
            "-r", "-R", "-i", "-n", "-l", "-c", "-v", "-w", "-F", "-E", "-e PATTERN", "--recursive", "--ignore-case", "--line-number",
            "--files-with-matches", "--count", "--invert-match", "--word-regexp", "--fixed-strings", "--extended-regexp", "--regexp=PATTERN",
            "-A N", "-B N", "-C N", "--after-context=N", "--before-context=N", "--context=N",
        ],
        Tool::Tree => &["-L N", "-d", "-a"],
        Tool::Sed => &["-e SCRIPT", "-i", "-n", "-E", "-r", "--expression=SCRIPT", "--in-place", "--quiet", "--silent", "--regexp-extended"],
        Tool::Find => &["-name P", "-iname P", "-path P", "-type f|d", "-maxdepth N", "-mindepth N", "-print", "-exec CMD {} ;", "-exec CMD {} +"],
    };
    s.iter().map(|x| x.to_string()).collect()
}

/// Option fields (JSON names) each tool accepts in the typed form.
fn fields(tool: Tool) -> &'static [&'static str] {
    match tool {
        Tool::Ls => &["long", "all", "recursive"],
        Tool::Cat | Tool::Stat => &[],
        Tool::Head => &["lines", "bytes"],
        Tool::Tail => &["lines", "bytes", "fromStart"],
        Tool::Wc => &["countLines", "countWords", "countBytes"],
        Tool::Grep => &["patterns", "recursive", "ignoreCase", "lineNumber", "filesWithMatches", "count", "invert", "word", "fixed", "extended", "afterContext", "beforeContext"],
        Tool::Find => &["tests", "maxdepth", "mindepth", "exec"],
        Tool::Tree => &["level", "dirsOnly", "all"],
        Tool::Sed => &["scripts", "inPlace", "quiet", "extended"],
    }
}

/// Check that a typed call sets only options of its tool.
pub fn check(call: &Call) -> Result<(), ToolError> {
    let set = serde_json::to_value(&call.req.options).unwrap_or_default();
    let default = serde_json::to_value(Options::default()).unwrap_or_default();
    let allowed = fields(call.tool);
    for (k, v) in set.as_object().into_iter().flatten() {
        if default.get(k) != Some(v) && !allowed.contains(&k.as_str()) {
            return Err(ToolError { error_id: "tool.unknown_option".into(), message: format!("{}: unsupported option '{k}'", call.tool.name()), supported: allowed.iter().map(|s| s.to_string()).collect() });
        }
    }
    if call.tool == Tool::Grep && call.req.options.patterns.is_empty() {
        return Err(err(call.tool, "tool.missing_operand", "grep: no pattern given".into()));
    }
    if call.tool == Tool::Sed && call.req.options.scripts.is_empty() {
        return Err(err(call.tool, "tool.missing_operand", "sed: no script given".into()));
    }
    Ok(())
}

fn err(tool: Tool, id: &str, msg: String) -> ToolError {
    ToolError { error_id: id.into(), message: msg, supported: supported(tool) }
}

fn unknown(tool: Tool, opt: &str) -> ToolError {
    err(tool, "tool.unknown_option", format!("{}: unsupported option '{opt}'", tool.name()))
}

fn number(tool: Tool, v: &str, plus_ok: bool, o: &mut Options) -> Result<u64, ToolError> {
    let (plus, digits) = match v.strip_prefix('+') {
        Some(d) if plus_ok => (true, d),
        _ => (false, v),
    };
    let n = digits.parse::<u64>().map_err(|_| err(tool, "tool.bad_number", format!("{}: invalid number '{v}'", tool.name())))?;
    if plus {
        o.from_start = true;
    }
    Ok(n)
}

/// Parse an argv (`argv[0]` is the tool name, also as `base-<tool>` or a path).
pub fn parse(argv: &[String]) -> Result<Call, ToolError> {
    let name = argv.first().map(|s| s.as_str()).unwrap_or("");
    let tool = Tool::from_name(name).ok_or_else(|| ToolError {
        error_id: "tool.unknown".into(),
        message: format!("unknown tool '{name}'"),
        supported: TOOLS.iter().map(|s| s.to_string()).collect(),
    })?;
    if tool == Tool::Find {
        return parse_find(&argv[1..]);
    }
    let mut o = Options::default();
    let mut operands: Vec<String> = vec![];
    let args = &argv[1..];
    let mut i = 0;
    let mut opts_done = false;
    while i < args.len() {
        let a = &args[i];
        i += 1;
        if opts_done || a == "-" || !a.starts_with('-') {
            operands.push(a.clone());
            continue;
        }
        if a == "--" {
            opts_done = true;
            continue;
        }
        if let Some(long) = a.strip_prefix("--") {
            let (k, v) = match long.split_once('=') {
                Some((k, v)) => (k, Some(v.to_string())),
                None => (long, None),
            };
            let flag = |o: &mut Options, k: &str| -> bool {
                match (tool, k) {
                    (Tool::Ls, "all") => o.all = true,
                    (Tool::Ls, "recursive") | (Tool::Grep, "recursive") => o.recursive = true,
                    (Tool::Wc, "lines") => o.count_lines = true,
                    (Tool::Wc, "words") => o.count_words = true,
                    (Tool::Wc, "bytes") => o.count_bytes = true,
                    (Tool::Grep, "ignore-case") => o.ignore_case = true,
                    (Tool::Grep, "line-number") => o.line_number = true,
                    (Tool::Grep, "files-with-matches") => o.files_with_matches = true,
                    (Tool::Grep, "count") => o.count = true,
                    (Tool::Grep, "invert-match") => o.invert = true,
                    (Tool::Grep, "word-regexp") => o.word = true,
                    (Tool::Grep, "fixed-strings") => o.fixed = true,
                    (Tool::Grep, "extended-regexp") | (Tool::Sed, "regexp-extended") => o.extended = true,
                    (Tool::Sed, "in-place") => o.in_place = true,
                    (Tool::Sed, "quiet") | (Tool::Sed, "silent") => o.quiet = true,
                    _ => return false,
                }
                true
            };
            match (tool, k, v) {
                (Tool::Head | Tool::Tail, "lines", Some(v)) => o.lines = Some(number(tool, &v, tool == Tool::Tail, &mut o)?),
                (Tool::Head | Tool::Tail, "bytes", Some(v)) => o.bytes = Some(number(tool, &v, tool == Tool::Tail, &mut o)?),
                (Tool::Grep, "regexp", Some(v)) => o.patterns.push(v),
                (Tool::Grep, "after-context", Some(v)) => o.after_context = Some(number(tool, &v, false, &mut o)?),
                (Tool::Grep, "before-context", Some(v)) => o.before_context = Some(number(tool, &v, false, &mut o)?),
                (Tool::Grep, "context", Some(v)) => {
                    let n = number(tool, &v, false, &mut o)?;
                    o.after_context = Some(n);
                    o.before_context = Some(n);
                }
                (Tool::Sed, "expression", Some(v)) => o.scripts.push(v),
                (_, k, None) if flag(&mut o, k) => {}
                _ => return Err(unknown(tool, a)),
            }
            continue;
        }
        // head -5 / tail -5
        if matches!(tool, Tool::Head | Tool::Tail) && a[1..].chars().all(|c| c.is_ascii_digit()) {
            o.lines = Some(number(tool, &a[1..], false, &mut o)?);
            continue;
        }
        let chars: Vec<char> = a[1..].chars().collect();
        let mut j = 0;
        while j < chars.len() {
            let c = chars[j];
            j += 1;
            // options with a value: rest of the cluster or the next argument
            let takes_value = matches!(
                (tool, c),
                (Tool::Head | Tool::Tail, 'n') | (Tool::Head | Tool::Tail, 'c') | (Tool::Grep, 'e') | (Tool::Grep, 'A') | (Tool::Grep, 'B') | (Tool::Grep, 'C') | (Tool::Tree, 'L') | (Tool::Sed, 'e')
            );
            if takes_value {
                let rest: String = chars[j..].iter().collect();
                let v = if !rest.is_empty() {
                    rest
                } else if i < args.len() {
                    i += 1;
                    args[i - 1].clone()
                } else {
                    return Err(err(tool, "tool.missing_value", format!("{}: option '-{c}' requires a value", tool.name())));
                };
                j = chars.len();
                match (tool, c) {
                    (_, 'n') => o.lines = Some(number(tool, &v, tool == Tool::Tail, &mut o)?),
                    (_, 'c') => o.bytes = Some(number(tool, &v, tool == Tool::Tail, &mut o)?),
                    (_, 'A') => o.after_context = Some(number(tool, &v, false, &mut o)?),
                    (_, 'B') => o.before_context = Some(number(tool, &v, false, &mut o)?),
                    (_, 'C') => {
                        let n = number(tool, &v, false, &mut o)?;
                        o.after_context = Some(n);
                        o.before_context = Some(n);
                    }
                    (_, 'L') => o.level = Some(number(tool, &v, false, &mut o)? as u32),
                    (Tool::Sed, 'e') => o.scripts.push(v),
                    _ => o.patterns.push(v),
                }
                continue;
            }
            match (tool, c) {
                (Tool::Ls, 'l') => o.long = true,
                (Tool::Ls, 'a') => o.all = true,
                (Tool::Ls, '1') => {}
                (Tool::Ls, 'R') | (Tool::Grep, 'r') | (Tool::Grep, 'R') => o.recursive = true,
                (Tool::Wc, 'l') => o.count_lines = true,
                (Tool::Wc, 'w') => o.count_words = true,
                (Tool::Wc, 'c') => o.count_bytes = true,
                (Tool::Grep, 'i') => o.ignore_case = true,
                (Tool::Grep, 'n') => o.line_number = true,
                (Tool::Grep, 'l') => o.files_with_matches = true,
                (Tool::Grep, 'c') => o.count = true,
                (Tool::Grep, 'v') => o.invert = true,
                (Tool::Grep, 'w') => o.word = true,
                (Tool::Grep, 'F') => o.fixed = true,
                (Tool::Grep, 'E') | (Tool::Sed, 'E') | (Tool::Sed, 'r') => o.extended = true,
                (Tool::Tree, 'd') => o.dirs_only = true,
                (Tool::Tree, 'a') => o.all = true,
                (Tool::Sed, 'i') => o.in_place = true,
                (Tool::Sed, 'n') => o.quiet = true,
                _ => return Err(unknown(tool, &format!("-{c}"))),
            }
        }
    }
    if tool == Tool::Grep && o.patterns.is_empty() {
        if operands.is_empty() {
            return Err(err(tool, "tool.missing_operand", "grep: no pattern given".into()));
        }
        o.patterns.push(operands.remove(0));
    }
    if tool == Tool::Sed && o.scripts.is_empty() {
        if operands.is_empty() {
            return Err(err(tool, "tool.missing_operand", "sed: no script given".into()));
        }
        o.scripts.push(operands.remove(0));
    }
    if matches!(tool, Tool::Cat | Tool::Head | Tool::Tail | Tool::Wc | Tool::Stat | Tool::Sed) && operands.is_empty() {
        return Err(err(tool, "tool.missing_operand", format!("{}: missing file operand (standard input is not supported)", tool.name())));
    }
    Ok(Call { tool, req: Request { cwd: String::new(), paths: operands, display: vec![], options: o } })
}

fn parse_find(args: &[String]) -> Result<Call, ToolError> {
    let tool = Tool::Find;
    let mut o = Options::default();
    let mut paths = vec![];
    let mut i = 0;
    while i < args.len() && !args[i].starts_with('-') && args[i] != "(" && args[i] != "!" {
        paths.push(args[i].clone());
        i += 1;
    }
    let value = |i: &mut usize, opt: &str| -> Result<String, ToolError> {
        *i += 1;
        args.get(*i - 1).cloned().ok_or_else(|| err(tool, "tool.missing_value", format!("find: missing argument to '{opt}'")))
    };
    while i < args.len() {
        let a = args[i].clone();
        i += 1;
        match a.as_str() {
            "-name" => o.tests.push(FindTest::Name(value(&mut i, &a)?)),
            "-iname" => o.tests.push(FindTest::Iname(value(&mut i, &a)?)),
            "-path" | "-wholename" => o.tests.push(FindTest::Path(value(&mut i, &a)?)),
            "-type" => {
                let t = value(&mut i, &a)?;
                if t != "f" && t != "d" {
                    return Err(err(tool, "tool.bad_value", format!("find: unsupported type '{t}'")));
                }
                o.tests.push(FindTest::Type(t));
            }
            "-maxdepth" | "-mindepth" => {
                let v = value(&mut i, &a)?;
                let n = v.parse::<u32>().map_err(|_| err(tool, "tool.bad_number", format!("find: invalid number '{v}'")))?;
                if a == "-maxdepth" {
                    o.maxdepth = Some(n)
                } else {
                    o.mindepth = Some(n)
                }
            }
            "-print" => {}
            "-exec" => {
                let mut argv = vec![];
                let mut batch = None;
                while i < args.len() {
                    let x = args[i].clone();
                    i += 1;
                    if x == ";" {
                        batch = Some(false);
                        break;
                    }
                    if x == "+" && argv.last().map(|l: &String| l == "{}").unwrap_or(false) {
                        batch = Some(true);
                        break;
                    }
                    argv.push(x);
                }
                let batch = batch.ok_or_else(|| err(tool, "tool.missing_value", "find: missing argument to '-exec'".into()))?;
                if argv.is_empty() || !argv.iter().any(|x| x.contains("{}")) {
                    return Err(err(tool, "tool.bad_value", "find: -exec needs a command and {}".into()));
                }
                if o.exec.is_some() {
                    return Err(err(tool, "tool.unknown_option", "find: only one -exec is supported".into()));
                }
                o.exec = Some(Exec { argv, batch });
            }
            _ => return Err(unknown(tool, &a)),
        }
    }
    Ok(Call { tool, req: Request { cwd: String::new(), paths, display: vec![], options: o } })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<Call, ToolError> {
        parse(&s.split_whitespace().map(String::from).collect::<Vec<_>>())
    }

    #[test]
    fn grep_options_mixed_with_operands() {
        let c = p("grep -rn Rechnung /mailbox -i").unwrap();
        assert_eq!(c.tool, Tool::Grep);
        assert!(c.req.options.recursive && c.req.options.line_number && c.req.options.ignore_case);
        assert_eq!(c.req.options.patterns, vec!["Rechnung"]);
        assert_eq!(c.req.paths, vec!["/mailbox"]);
        let c = p("i3cm-grep -e a -e b x").unwrap();
        assert_eq!(c.req.options.patterns, vec!["a", "b"]);
        assert_eq!(c.req.paths, vec!["x"]);
    }

    #[test]
    fn unknown_options_are_errors_with_the_list() {
        let e = p("grep -P x y").unwrap_err();
        assert_eq!(e.error_id, "tool.unknown_option");
        assert!(e.supported.contains(&"-r".to_string()));
        assert!(e.to_string().contains("unsupported option '-P'"));
        assert_eq!(p("ls --color=auto").unwrap_err().error_id, "tool.unknown_option");
        assert_eq!(p("find . -newer x").unwrap_err().error_id, "tool.unknown_option");
        assert_eq!(p("cat").unwrap_err().error_id, "tool.missing_operand");
    }

    #[test]
    fn head_tail_numbers() {
        assert_eq!(p("head -5 a").unwrap().req.options.lines, Some(5));
        assert_eq!(p("head -n3 a").unwrap().req.options.lines, Some(3));
        let t = p("tail -n +2 a").unwrap();
        assert_eq!((t.req.options.lines, t.req.options.from_start), (Some(2), true));
        assert_eq!(p("head -n x a").unwrap_err().error_id, "tool.bad_number");
        assert_eq!(p("head --bytes=4 a").unwrap().req.options.bytes, Some(4));
    }

    #[test]
    fn find_expressions() {
        let c = p("find a b -maxdepth 2 -name *.md -type f -exec grep -l x {} +").unwrap();
        assert_eq!(c.req.paths, vec!["a", "b"]);
        assert_eq!(c.req.options.maxdepth, Some(2));
        assert_eq!(c.req.options.tests, vec![FindTest::Name("*.md".into()), FindTest::Type("f".into())]);
        assert_eq!(c.req.options.exec, Some(Exec { argv: vec!["grep".into(), "-l".into(), "x".into(), "{}".into()], batch: true }));
        assert!(p("find -name x -exec cat {} ;").unwrap().req.options.exec.is_some());
    }
}
