//! Chat templates applied to sample conversations, to compare the prompts a model
//! actually sees: two templates can differ in text and give the same prompt, or
//! differ in one space that changes it.
//!
//! The templates are Jinja, run with minijinja under the settings Hugging Face
//! uses (`trim_blocks`, `lstrip_blocks`, Python string methods, a `tojson` that
//! writes like Python's `json.dumps`).

use std::fmt::Write as _;

use minijinja::value::{Kwargs, Value};
use minijinja::{Environment, ErrorKind};
use serde_json::json;

use super::gguf::Gguf;
use crate::Result;

/// Template steps allowed per render; a template that loops forever stops here.
const FUEL: u64 = 20_000_000;

struct Scenario {
    title: &'static str,
    context: serde_json::Value,
    tools: bool,
}

fn scenarios() -> Vec<Scenario> {
    let system = json!({"role": "system", "content": "You are a helpful assistant."});
    let user = json!({"role": "user", "content": "What is the capital of France?"});
    let assistant = json!({"role": "assistant", "content": "The capital of France is Paris."});
    let user2 = json!({"role": "user", "content": "And of Germany?"});
    let tools = json!([{
        "type": "function",
        "function": {
            "name": "get_weather",
            "description": "Current weather for a city",
            "parameters": {
                "type": "object",
                "properties": {"city": {"type": "string", "description": "Name of the city"}},
                "required": ["city"]
            }
        }
    }]);
    vec![
        Scenario {
            title: "System and user message, ready for the reply",
            context: json!({"messages": [system, user], "add_generation_prompt": true}),
            tools: false,
        },
        Scenario {
            title: "User message only",
            context: json!({"messages": [user], "add_generation_prompt": true}),
            tools: false,
        },
        Scenario {
            title: "Several turns",
            context: json!({"messages": [system, user, assistant, user2], "add_generation_prompt": true}),
            tools: false,
        },
        Scenario {
            title: "Finished conversation, no generation prompt",
            context: json!({"messages": [system, user, assistant], "add_generation_prompt": false}),
            tools: false,
        },
        Scenario {
            title: "Tool call and tool result",
            context: json!({
                "tools": tools,
                "messages": [
                    system,
                    {"role": "user", "content": "What is the weather in Berlin?"},
                    {"role": "assistant", "content": "", "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "get_weather", "arguments": {"city": "Berlin"}}
                    }]},
                    {"role": "tool", "tool_call_id": "call_1", "name": "get_weather",
                     "content": "{\"temperature\": 21, \"sky\": \"sunny\"}"}
                ],
                "add_generation_prompt": true
            }),
            tools: true,
        },
    ]
}

fn environment() -> Environment<'static> {
    let mut env = Environment::new();
    env.set_trim_blocks(true);
    env.set_lstrip_blocks(true);
    env.set_fuel(Some(FUEL));
    env.set_unknown_method_callback(minijinja_contrib::pycompat::unknown_method_callback);
    env.add_function("raise_exception", |message: String| -> std::result::Result<Value, minijinja::Error> {
        Err(minijinja::Error::new(ErrorKind::InvalidOperation, format!("raise_exception: {message}")))
    });
    env.add_function("strftime_now", strftime_now);
    env.add_filter("tojson", tojson);
    env
}

/// The current date, fixed so that both sides render alike: Wednesday, 15 January 2025, 09:30.
fn strftime_now(format: String) -> String {
    let mut out = String::new();
    let mut chars = format.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let mut spec = chars.next().unwrap_or('%');
        let no_pad = spec == '-';
        if no_pad {
            spec = chars.next().unwrap_or('%');
        }
        out.push_str(match (spec, no_pad) {
            ('d', false) => "15",
            ('d', true) => "15",
            ('m', false) => "01",
            ('m', true) => "1",
            ('Y', _) => "2025",
            ('y', _) => "25",
            ('B', _) => "January",
            ('b', _) => "Jan",
            ('A', _) => "Wednesday",
            ('a', _) => "Wed",
            ('H', false) => "09",
            ('H', true) => "9",
            ('M', _) => "30",
            ('S', _) => "00",
            ('j', _) => "015",
            ('%', _) => "%",
            _ => {
                out.push('%');
                out.push(spec);
                continue;
            }
        });
    }
    out
}

/// Writes JSON separated like Python's `json.dumps`: `", "` and `": "`.
struct PythonStyle;

impl serde_json::ser::Formatter for PythonStyle {
    fn begin_array_value<W: ?Sized + std::io::Write>(&mut self, w: &mut W, first: bool) -> std::io::Result<()> {
        if first { Ok(()) } else { w.write_all(b", ") }
    }

    fn begin_object_key<W: ?Sized + std::io::Write>(&mut self, w: &mut W, first: bool) -> std::io::Result<()> {
        if first { Ok(()) } else { w.write_all(b", ") }
    }

    fn begin_object_value<W: ?Sized + std::io::Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        w.write_all(b": ")
    }
}

fn tojson(value: Value, kwargs: Kwargs) -> std::result::Result<Value, minijinja::Error> {
    use serde::Serialize as _;
    let indent: Option<usize> = kwargs.get("indent")?;
    kwargs.assert_all_used()?;
    let mut out = Vec::new();
    let failed = |e: serde_json::Error| minijinja::Error::new(ErrorKind::InvalidOperation, e.to_string());
    match indent {
        Some(n) => {
            let spaces = " ".repeat(n);
            let formatter = serde_json::ser::PrettyFormatter::with_indent(spaces.as_bytes());
            value.serialize(&mut serde_json::Serializer::with_formatter(&mut out, formatter)).map_err(failed)?;
        }
        None => value.serialize(&mut serde_json::Serializer::with_formatter(&mut out, PythonStyle)).map_err(failed)?,
    }
    Ok(Value::from(String::from_utf8_lossy(&out).into_owned()))
}

fn error_text(e: &minijinja::Error) -> String {
    let mut text = e.to_string();
    let mut source = std::error::Error::source(e);
    while let Some(s) = source {
        let _ = write!(text, ": {s}");
        source = s.source();
    }
    text.replace('\n', " ")
}

/// Every sample conversation rendered with the file's chat template. A blank line
/// before the end marker means the prompt ends with a line break.
pub fn preview(data: &[u8]) -> Result<String> {
    let g = Gguf::parse(data)?;
    let templates = g.chat_templates();
    let Some(&(_, default)) = templates.iter().find(|(n, _)| n.is_empty()).or(templates.first()) else {
        return Ok("! This file has no chat template.\n".into());
    };
    let tool_template = templates.iter().find(|(n, _)| *n == "tool_use").map(|(_, t)| *t);
    let token = |key: &str| g.get(key).and_then(|v| v.as_u64()).and_then(|id| g.token(id)).unwrap_or("");
    let special = json!({
        "bos_token": token("tokenizer.ggml.bos_token_id"),
        "eos_token": token("tokenizer.ggml.eos_token_id"),
        "pad_token": token("tokenizer.ggml.padding_token_id"),
        "unk_token": token("tokenizer.ggml.unknown_token_id"),
    });

    let env = environment();
    let mut out = String::new();
    let _ = writeln!(out, "=== Special tokens ===");
    for (key, value) in special.as_object().expect("an object") {
        let _ = writeln!(out, "{key} = {value}");
    }
    for scenario in scenarios() {
        // As in Hugging Face: a `tool_use` template, when there is one, handles tools.
        let (source, which) = match tool_template {
            Some(t) if scenario.tools => (t, " (template tool_use)"),
            _ => (default, ""),
        };
        let _ = writeln!(out, "\n=== {}{which} ===", scenario.title);
        let mut context = special.clone();
        context.as_object_mut().expect("an object").extend(scenario.context.as_object().expect("an object").clone());
        let rendered = env.template_from_str(source).and_then(|t| t.render(Value::from_serialize(&context)));
        match rendered {
            Ok(prompt) => {
                out.push_str(&prompt);
                out.push('\n');
            }
            Err(e) => {
                let _ = writeln!(out, "! {}", error_text(&e));
            }
        }
        let _ = writeln!(out, "--- end of prompt ---");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::gguf::tests::sample;
    use super::*;

    const CHATML: &str = "{% for message in messages %}{{'<|im_start|>' + message['role'] + '\n' + message['content'] + '<|im_end|>' + '\n'}}{% endfor %}{% if add_generation_prompt %}{{ '<|im_start|>assistant\n' }}{% endif %}";

    #[test]
    fn renders_sample_conversations() {
        let out = preview(&sample(CHATML, 14)).unwrap();
        assert!(out.contains("bos_token = \"<s>\"\neos_token = \"<|im_end|>\""), "{out}");
        assert!(
            out.contains(
                "=== User message only ===\n<|im_start|>user\nWhat is the capital of France?<|im_end|>\n<|im_start|>assistant\n\n--- end of prompt ---"
            ),
            "{out}"
        );
    }

    #[test]
    fn python_features_errors_and_tools() {
        let template = "{%- if messages[0].role != 'system' %}{{ raise_exception('needs a system message') }}{% endif -%}\
            {{ messages[0].content.strip().upper() }}|{{ strftime_now('%d %b %Y') }}|\
            {%- if tools %}{{ tools[0].function.parameters | tojson }}|{{ messages[2].tool_calls[0].function.arguments | tojson }}{% endif %}";
        let out = preview(&sample(template, 14)).unwrap();
        assert!(out.contains("YOU ARE A HELPFUL ASSISTANT.|15 Jan 2025|\n"), "{out}");
        assert!(out.contains("! "), "{out}");
        assert!(out.contains("needs a system message"), "{out}");
        assert!(out.contains(r#"{"type": "object", "properties": {"city": {"type": "string", "description": "Name of the city"}}, "required": ["city"]}|{"city": "Berlin"}"#), "{out}");
    }

    /// Constructs of real templates (Llama 3.1, Qwen3, DeepSeek-R1) that a Jinja
    /// engine other than Python's might not support.
    #[test]
    fn constructs_of_real_templates() {
        let template = r#"{{- bos_token }}
{%- if not date_string is defined %}{%- set date_string = strftime_now("%d %b %Y") %}{%- endif %}
{%- if messages[0]['role'] == 'system' %}{%- set system_message = messages[0]['content']|trim %}{%- set messages = messages[1:] %}{%- else %}{%- set system_message = "" %}{%- endif %}
{%- set ns = namespace(multi_step_tool=true, last_query_index=messages|length - 1) %}
{%- for message in messages[::-1] %}
    {%- set index = (messages|length - 1) - loop.index0 %}
    {%- if ns.multi_step_tool and message.role == "user" and message.content is string and not message.content.startswith('<tool_response>') %}
        {%- set ns.multi_step_tool = false %}
        {%- set ns.last_query_index = index %}
    {%- endif %}
{%- endfor %}
[sys:{{ system_message }}|date:{{ date_string }}|last:{{ ns.last_query_index }}]
{%- if tools is not none and tools is defined %}{%- for t in tools %}{{ t.function.parameters | tojson(indent=4) }}{%- endfor %}{%- endif %}
{%- for message in messages %}
    {%- if 'tool_calls' in message %}<call {{ message.tool_calls[0].function.arguments | tojson }}>
    {%- elif message.content is mapping %}<map>
    {%- else %}{%- set content = message.content %}{%- if '</think>' in content %}{%- set content = content.split('</think>')[-1].lstrip('
') %}{%- endif %}<{{ message.role }}:{{ content.rstrip() }}>
    {%- endif %}
{%- endfor %}"#;
        let out = preview(&sample(template, 14)).unwrap();
        assert!(!out.contains("\n! "), "{out}");
        assert!(
            out.contains(
                "<s>[sys:You are a helpful assistant.|date:15 Jan 2025|last:0]<user:What is the capital of France?>"
            ),
            "{out}"
        );
        assert!(out.contains("[sys:|date:15 Jan 2025|last:0]"), "{out}");
        assert!(out.contains("\"properties\": {\n        \"city\": {"), "{out}");
        assert!(out.contains("<call {\"city\": \"Berlin\"}>"), "{out}");
    }

    #[test]
    fn runaway_templates_stop() {
        let out =
            preview(&sample("{% for i in range(100000) %}{% for j in range(100000) %}{% endfor %}{% endfor %}", 14))
                .unwrap();
        assert!(out.contains("! "), "{out}");
    }
}
