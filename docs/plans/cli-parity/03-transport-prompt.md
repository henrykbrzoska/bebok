# Faza 3 — system prompt świadomy transportu

Typ: `fix:`. Zależności: brak (z 04/07 wariant `bebok_tools` nabiera znaczenia).

## API

- `bebok-llm/src/provider.rs`:
  `enum Transport { Http, AgenticCli { bebok_tools: bool, native_tools: bool } }`,
  `trait Provider { fn transport(&self) -> Transport { Transport::Http } }`.
- `CliProvider::transport()`: `bebok_tools` = most MCP (04) aktywny,
  `native_tools` = profil pozwala na narzędzia CLI.

## Składanie promptu

- `bebok-server/src/services/turn.rs::assemble_prompt` + `RequestBuilder`
  w `bebok-core/src/agent/request.rs` dostają `Transport`.
- Sekcje i reguła:

| Sekcja | Http | CLI bez bebok_tools | CLI z bebok_tools |
|---|---|---|---|
| AGENTS.md / skille | tak | **nie** — idą przez profil (07) | nie (profil) |
| host OS / bash | tak | nie | tak, nazwy `mcp__bebok__*` |
| verify_prompt | tak | nie | tak |
| build_test_prompt | tak | tak (polityka, nie narzędzie) | tak |
| code_graph / index_prompt | tak | nie | tak, przemianowane |
| delegacja (fleet/task) | orchestrator | nigdy | nigdy |

- helper `tool_ref(name, transport) -> String` w `bebok-core/src/agent/prompt_env.rs`
  (albo nowy `prompt_tools.rs`); wszystkie sekcje używają go zamiast literałów.
- `agent/task_tool.rs` i fleet: dziecko na providerze CLI → ten sam warunek.

## Testy

- snapshot promptu dla 3 transportów (`insta` jeśli jest w workspace,
  inaczej asercje `contains`/`!contains`).

## Pliki

`bebok-llm/src/provider.rs`, `bebok-cli-agents/src/provider.rs`,
`bebok-core/src/agent/{request.rs,index_prompt.rs,verify_prompt.rs,
code_graph_prompt.rs,prompt_env.rs,task_tool.rs}`,
`bebok-server/src/services/turn.rs`.
