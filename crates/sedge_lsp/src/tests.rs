use super::*;

fn open(server: &mut Server, text: &str) -> Vec<Json> {
    let msg = Json::parse(&format!(
        r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"file:///w/main.sg","languageId":"sedge","version":1,"text":{}}}}}}}"#,
        Json::str(text)
    ))
    .unwrap();
    server.handle(&msg)
}

fn request(server: &mut Server, method: &str, line: u32, character: u32) -> Json {
    let msg = Json::parse(&format!(
        r#"{{"jsonrpc":"2.0","id":7,"method":"{method}","params":{{"textDocument":{{"uri":"file:///w/main.sg"}},"position":{{"line":{line},"character":{character}}},"context":{{"includeDeclaration":true}}}}}}"#
    ))
    .unwrap();
    let mut replies = server.handle(&msg);
    assert_eq!(replies.len(), 1);
    replies.remove(0).get("result").clone()
}

const SRC: &str = "/// A 2D point.
struct Point { x: Float, y: Float }

impl Point {
    /// Distance from the origin.
    fn norm(self) -> Float { (self.x * self.x + self.y * self.y).sqrt() }
}

fn main() {
    let p = Point { x: 3.0, y: 4.0 }
    print(p.norm())
    let n = p.x
}
";

#[test]
fn publishes_diagnostics() {
    let mut s = Server::default();
    let replies = open(&mut s, "fn main() {\n    let x: Int = \"é\"\n}\n");
    let d = &replies[0];
    assert_eq!(
        d.get("method").as_str(),
        Some("textDocument/publishDiagnostics")
    );
    let Json::Arr(diags) = d.at(&["params", "diagnostics"]) else {
        panic!()
    };
    assert_eq!(diags.len(), 1);
    let range = diags[0].get("range");
    assert_eq!(range.at(&["start", "line"]).as_u32(), Some(1));
    assert_eq!(range.at(&["start", "character"]).as_u32(), Some(17));
    assert_eq!(
        range.at(&["end", "character"]).as_u32(),
        Some(20),
        "UTF-16 columns"
    );
    assert!(diags[0]
        .get("message")
        .as_str()
        .unwrap()
        .contains("mismatched types"));
}

#[test]
fn hover_shows_types_and_docs() {
    let mut s = Server::default();
    open(&mut s, SRC);
    // `p` in `print(p.norm())`
    let h = request(&mut s, "textDocument/hover", 10, 10);
    let v = h.at(&["contents", "value"]).as_str().unwrap();
    assert!(v.contains("let p: Point"), "{v}");
    // `norm`
    let h = request(&mut s, "textDocument/hover", 10, 13);
    let v = h.at(&["contents", "value"]).as_str().unwrap();
    assert!(v.contains("fn Point.norm(self) -> Float"), "{v}");
    assert!(v.contains("Distance from the origin."), "{v}");
    // `x` in `p.x`
    let h = request(&mut s, "textDocument/hover", 11, 14);
    let v = h.at(&["contents", "value"]).as_str().unwrap();
    assert!(v.contains("Point.x: Float"), "{v}");
}

#[test]
fn definition_and_references() {
    let mut s = Server::default();
    open(&mut s, SRC);
    let d = request(&mut s, "textDocument/definition", 10, 13);
    assert_eq!(d.at(&["range", "start", "line"]).as_u32(), Some(5));
    let r = request(&mut s, "textDocument/references", 9, 8);
    let Json::Arr(refs) = r else { panic!() };
    assert_eq!(refs.len(), 3, "the declaration of p and two uses");
}

#[test]
fn completes_members_from_last_good_version() {
    let mut s = Server::default();
    open(&mut s, SRC);
    // Now the user is typing `p.` and the file does not parse.
    let broken = SRC.replace("    let n = p.x\n", "    let n = p.\n");
    let msg = Json::parse(&format!(
        r#"{{"jsonrpc":"2.0","method":"textDocument/didChange","params":{{"textDocument":{{"uri":"file:///w/main.sg","version":2}},"contentChanges":[{{"text":{}}}]}}}}"#,
        Json::str(broken)
    ))
    .unwrap();
    s.handle(&msg);
    let c = request(&mut s, "textDocument/completion", 11, 14);
    let Json::Arr(items) = c else { panic!() };
    let labels: Vec<&str> = items
        .iter()
        .filter_map(|i| i.get("label").as_str())
        .collect();
    assert!(
        labels.contains(&"x") && labels.contains(&"norm"),
        "{labels:?}"
    );
}

#[test]
fn document_symbols() {
    let mut s = Server::default();
    open(&mut s, SRC);
    let r = request(&mut s, "textDocument/documentSymbol", 0, 0);
    let Json::Arr(syms) = r else { panic!() };
    let names: Vec<&str> = syms.iter().filter_map(|x| x.get("name").as_str()).collect();
    assert_eq!(names, ["Point", "impl Point", "main"]);
}

#[test]
fn stdio_framing() {
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
    let exit = r#"{"jsonrpc":"2.0","method":"exit"}"#;
    let input = format!(
        "Content-Length: {}\r\n\r\n{}Content-Length: {}\r\n\r\n{}",
        init.len(),
        init,
        exit.len(),
        exit
    );
    let mut out = Vec::new();
    serve(&mut input.as_bytes(), &mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(text.starts_with("Content-Length: "));
    assert!(text.contains("\"hoverProvider\":true"));
}
