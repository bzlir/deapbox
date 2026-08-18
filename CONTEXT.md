# deapbox

A single-binary bridge that lets the Operator remote-drive local coding-agent CLIs from a Feishu group chat. One Chat binds to one (Agent + Workspace); the Agent runs in the Workspace directory and its output is rendered back into the Chat.

## Language

**Operator**:
The single user who owns the machine running deapbox. Drives Agents remotely via Feishu when not at the keyboard.
_Avoid_: User, account, tenant, owner

**Workspace**:
A local project directory on the Operator's machine where an Agent runs. The Agent's working directory; one Workspace per Chat.
_Avoid_: project, repo, work_dir, cwd, codebase

**Chat**:
A Feishu conversation (group or p2p) bound to one (Agent + Workspace). One Chat is the unit of session continuity and resume; both group and p2p chats are identified by `chat_id`.
_Avoid_: conversation, room, channel, group

**Agent**:
A coding-agent CLI spawned locally by deapbox to work in a Workspace. Classified by AgentKind.
_Avoid_: model, llm, bot (reserved for the deapbox Feishu app itself), assistant, runner

**AgentKind**:
The variety of coding-agent CLI behind an Agent. Values: echo (test stub), claude-code, kimi-code; planned: opencode, codex.
_Avoid_: type, variant, flavor

**Binding**:
The (Agent, Workspace) pair a Chat is bound to. Cold state — changed only by binding commands (e.g. /switch), persisted across restarts.
_Avoid_: session, mapping, association, link

**Turn**:
One round of Operator message → Agent processing → Agent reply. The unit of serial execution within a Chat; the next Turn cannot start until the current one ends.
_Avoid_: request, iteration, round, message

**TurnEnd**:
The signal that marks a Turn's completion, emitted by the Agent itself (not a host-side guess via idle-timeout or EOF). For stream-json kinds, this is the non-compaction `result` event; other protocols map their own turn-end signal to the same concept.
_Avoid_: completion, done, finish, response-end

**AgentEvent**:
A structured piece of an Agent's reply within a Turn. Kinds: Text (final reply), Thinking (reasoning trace), ToolCall (tool invocation), ToolResult (tool output), Error (agent-reported failure), TurnEnd (turn boundary).
_Avoid_: message, response, output, frame

**Attachment**:
An artifact attached to an inbound Feishu message from the Operator. Kinds today: Image (Feishu image_key, bytes lazily downloaded); file/video/audio reserved.
_Avoid_: file, media, payload, asset
