// SyntaxHighlighter.swift — a pure, nonisolated tokenizer that colorizes one
// source line into an AttributedString using the calm cool palette. Per-line
// highlighting keeps a LazyVStack editor responsive (only visible rows build).

import SwiftUI

enum CodeLang {
    case rust, json, toml, markdown, swift, plain

    var label: String {
        switch self {
        case .rust: return "Rust"
        case .json: return "JSON"
        case .toml: return "TOML"
        case .markdown: return "Markdown"
        case .swift: return "Swift"
        case .plain: return "Text"
        }
    }

    static func detect(_ path: String) -> CodeLang {
        let p = path.lowercased()
        if p.hasSuffix(".rs") { return .rust }
        if p.hasSuffix(".swift") { return .swift }
        if p.hasSuffix(".toml") || p.hasSuffix(".lock") { return .toml }
        if p.hasSuffix(".md") || p.hasSuffix(".markdown") { return .markdown }
        if p.hasSuffix(".json") || p.hasSuffix(".jsonl") { return .json }
        return .plain
    }

    var dotColor: Color {
        switch self {
        case .rust, .toml: return Theme.blue
        case .swift: return Theme.coral
        case .json: return Theme.gold
        case .markdown: return Theme.green
        case .plain: return Theme.muted
        }
    }
}

enum SyntaxHighlighter {
    private static let keywords: Set<String> = [
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
        "extern", "false", "fn", "for", "func", "if", "impl", "import", "in", "let", "loop",
        "match", "mod", "move", "mut", "nil", "pub", "ref", "return", "self", "Self", "static",
        "struct", "super", "trait", "true", "type", "unsafe", "use", "var", "where", "while",
        "guard", "switch", "case", "default", "extension", "protocol", "class", "private",
        "public", "internal", "some",
    ]

    static func line(_ text: String, lang: CodeLang) -> AttributedString {
        switch lang {
        case .rust, .swift: return code(text)
        case .json: return json(text)
        case .toml: return toml(text)
        case .markdown: return markdown(text)
        case .plain: return run(text, Theme.textSoft)
        }
    }

    private static func run(_ text: String, _ color: Color) -> AttributedString {
        var s = AttributedString(text)
        s.foregroundColor = color
        s.font = Theme.mono(12)
        return s
    }

    private static func code(_ text: String) -> AttributedString {
        var out = AttributedString("")
        let chars = Array(text)
        var i = 0
        while i < chars.count {
            let c = chars[i]
            if c == "/" && i + 1 < chars.count && chars[i + 1] == "/" {
                out.append(run(String(chars[i...]), Theme.faint))
                break
            }
            if c == "\"" {
                let start = i
                i += 1
                while i < chars.count {
                    if chars[i] == "\\" { i += 2; continue }
                    if chars[i] == "\"" { i += 1; break }
                    i += 1
                }
                out.append(run(String(chars[start..<min(i, chars.count)]), Theme.green))
                continue
            }
            if c.isNumber {
                let start = i
                while i < chars.count && (chars[i].isNumber || chars[i] == "." || chars[i] == "_") { i += 1 }
                out.append(run(String(chars[start..<i]), Theme.coral))
                continue
            }
            if c.isLetter || c == "_" {
                let start = i
                while i < chars.count && (chars[i].isLetter || chars[i].isNumber || chars[i] == "_") { i += 1 }
                let word = String(chars[start..<i])
                let color: Color
                if keywords.contains(word) {
                    color = Theme.violet
                } else if let first = word.first, first.isUppercase {
                    color = Theme.blue
                } else if i < chars.count && chars[i] == "(" {
                    color = Theme.accent
                } else {
                    color = Theme.textSoft
                }
                out.append(run(word, color))
                continue
            }
            out.append(run(String(c), Theme.muted))
            i += 1
        }
        return out
    }

    private static func json(_ text: String) -> AttributedString {
        var out = AttributedString("")
        let chars = Array(text)
        var i = 0
        while i < chars.count {
            let c = chars[i]
            if c == "\"" {
                let start = i
                i += 1
                while i < chars.count {
                    if chars[i] == "\\" { i += 2; continue }
                    if chars[i] == "\"" { i += 1; break }
                    i += 1
                }
                var j = i
                while j < chars.count && chars[j] == " " { j += 1 }
                let isKey = j < chars.count && chars[j] == ":"
                out.append(run(String(chars[start..<min(i, chars.count)]), isKey ? Theme.blue : Theme.green))
                continue
            }
            if c.isNumber || (c == "-" && i + 1 < chars.count && chars[i + 1].isNumber) {
                let start = i
                i += 1
                while i < chars.count && (chars[i].isNumber || chars[i] == ".") { i += 1 }
                out.append(run(String(chars[start..<i]), Theme.coral))
                continue
            }
            if c.isLetter {
                let start = i
                while i < chars.count && chars[i].isLetter { i += 1 }
                out.append(run(String(chars[start..<i]), Theme.violet))
                continue
            }
            out.append(run(String(c), Theme.muted))
            i += 1
        }
        return out
    }

    private static func toml(_ text: String) -> AttributedString {
        let trimmed = text.trimmingCharacters(in: .whitespaces)
        if trimmed.hasPrefix("#") { return run(text, Theme.faint) }
        if trimmed.hasPrefix("[") { return run(text, Theme.blue) }
        if let eq = text.firstIndex(of: "=") {
            var out = run(String(text[..<eq]), Theme.text)
            out.append(run("=", Theme.muted))
            out.append(run(String(text[text.index(after: eq)...]), Theme.green))
            return out
        }
        return run(text, Theme.textSoft)
    }

    private static func markdown(_ text: String) -> AttributedString {
        let trimmed = text.trimmingCharacters(in: .whitespaces)
        if trimmed.hasPrefix("#") { return run(text, Theme.accent) }
        if trimmed.hasPrefix("```") { return run(text, Theme.violet) }
        if trimmed.hasPrefix("- ") || trimmed.hasPrefix("* ") || trimmed.hasPrefix("> ") {
            return run(text, Theme.blue)
        }
        return run(text, Theme.textSoft)
    }
}
