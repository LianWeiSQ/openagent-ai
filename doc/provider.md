# Provider And Tool-Calling Plane

The provider plane converts model-specific transports into one runtime
contract. It owns request serialization, stream decoding, tool-call assembly,
capability negotiation, and provider error classification. The agent loop owns
turn progression and tool execution; it does not understand provider wire
formats.

## Data Flow

```text
ContextPack
  -> provider capability lookup
  -> tool policy negotiation
  -> provider request builder
  -> HTTP or SSE transport
  -> dialect-specific frame decoder
  -> ToolCallAssembler
  -> ProviderStreamEvent
  -> AgentLoop
```

The shared output contract is `ProviderStreamEvent`: text and reasoning
deltas, complete tool calls, usage and finish state, retry/fallback resets, or
a structured tool-call error. CLI and HTTP runtimes use the same request
builders and normalizers from `src/provider`.

## Native Structured Calls

OpenAI Chat Completions, OpenAI Responses, Anthropic Messages, and Gemini
`generateContent` are native dialects. Their adapters read tool calls only from
the provider's structured response fields:

- OpenAI Chat: `choices[].delta.tool_calls` or
  `choices[].message.tool_calls`;
- OpenAI Responses: function-call output items and argument delta events;
- Anthropic: `tool_use` content blocks and `input_json_delta` events;
- Gemini: `functionCall` parts with structured `args`.

Ordinary assistant text is never scanned for tool syntax in a native dialect.
For example, documentation containing a literal `<tool_call>` block remains
text. This prevents accidental tool execution and keeps parsing policy outside
the agent loop.

## Optional Text Dialects

Hermes XML, Qwen XML, DeepSeek markers, and Pythonic calls are explicit
compatibility dialects. They are enabled with the profile model option
`tool_call_dialect`; they are never selected by a global text heuristic.

`tool_call_dialect: native` selects the provider's native dialect from the
provider id and wire API. Specific values such as `hermes`, `qwen_xml`,
`deepseek`, and `pythonic` opt in to text parsing at the provider adapter.

Text dialect parsers must consume a complete recognized envelope and produce
the same normalized tool-call contract as native adapters. Malformed or
truncated envelopes fail closed.

## Frame Assembly

`ToolCallFrame` separates transport decoding from call validation:

- `Start` establishes stream identity, call id, and tool name;
- `Arguments` contributes JSON deltas, snapshots, or structured values;
- `End` finalizes one call.

`ToolCallAssembler` tracks calls by stream identity, so interleaved parallel
calls cannot mix arguments. It rejects conflicting identities, missing start
frames, non-object arguments, malformed JSON, duplicate completion, and
unfinished calls at end of stream. A failed assembly becomes
`ProviderStreamEvent::ToolCallError`; it is not downgraded to best-effort tool
input.

## Schema And Policy Negotiation

`ToolSchema` carries the input schema plus:

- `strict`, requesting provider-side strict schema validation;
- `output_schema`, describing a structured tool result contract;
- `parallel_safe`, declaring that the runtime may schedule the tool in
  parallel.

Profiles can request `tool_choice` and `parallel_tool_calls`. Before request
serialization, the provider plane intersects that request with a
`ProviderCapabilitySet`. Negotiation validates named tools, disables parallel
calls when either the provider or a selected tool cannot support them, and
records which strict or output schemas are effective.

Provider control options are consumed by this negotiation layer. They are not
copied as arbitrary keys into provider payloads.

## Native Provider Routing

OpenAI-compatible providers share Chat Completions and Responses builders.
Anthropic and Gemini have native request and response adapters, including
native authentication headers and message/tool-result shapes.

Gemini API keys are sent through `x-goog-api-key`, never in the request URL.
The Gemini endpoint is constructed as:

```text
<base-url>/models/<encoded-model>:generateContent
<base-url>/models/<encoded-model>:streamGenerateContent?alt=sse
```

Provider retries and model fallback stay at the runtime transport boundary.
Reset and fallback events clear partial output before a replacement attempt is
accepted.

## Extension Rules

Adding a provider or dialect requires:

1. declare its capability set;
2. implement request serialization without leaking runtime-only options;
3. decode native structures into frames and normalized events;
4. classify malformed and truncated calls as errors;
5. add non-streaming, streaming-fragment, parallel-call, and ordinary-text
   tests;
6. add CLI or HTTP fake-provider coverage for the public routing boundary.

No provider adapter may execute tools directly, and no product surface may
maintain a second tool-call parser.
