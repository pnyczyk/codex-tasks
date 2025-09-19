# Specyfikacja protokołu Codex

## Cel dokumentu
Niniejszy dokument opisuje wewnętrzny protokół wymiany komunikatów używany przez Codex CLI/TUI i serwer (`codex-core`). Zawiera kompletne definicje struktur JSON oraz przepływów potrzebnych do implementacji klienta, który łączy się z Codexem, wysyła polecenia i odbiera zdarzenia. Spec obejmuje zarówno kolejki `Submission`/`Event`, jak i interfejs JSON-RPC wykorzystywany przez warstwę MCP.

## Kanały komunikacyjne i serializacja
- Komunikacja między klientem a agentem zachodzi dwukierunkowo i asynchronicznie.
  - **Submission Queue (SQ)**: klient publikuje żądania w postaci obiektów `Submission`.
  - **Event Queue (EQ)**: agent (Codex) publikuje odpowiedzi i powiadomienia jako obiekty `Event`.
- Wszystkie struktury są serializowane do JSON przy użyciu `serde`. Enumy wykorzystują pola dyskryminujące `type` (albo `method`/`mode`) z wartościami w `snake_case` lub `camelCase`, zgodnie z atrybutami w kodzie.
- Część typów generuje definicje TypeScript (`ts-rs`). Dlatego pola i warianty muszą zachować stabilne nazwy.

### Identyfikatory i korelacja
- Każdy `Submission` ma unikalny (w obrębie sesji) `id: String`. Wartość jest zwracana w odpowiadających zdarzeniach `Event.id`.
- `Event.id` pozwala sparować sekwencję komunikatów z konkretnym żądaniem. Dodatkowe zdarzenia tła mogą również korzystać z ostatniego `id` lub własnych identyfikatorów ustalonych przez serwer.
- MCP korzysta z `RequestId` (liczby całkowite lub string) zgodnie z JSON-RPC 2.0.

### Konwencje serializacji
- `Duration` (np. w `ExecCommandEndEvent.duration`) serializuje się jako obiekt `{ "secs": u64, "nanos": u32 }`, zgodnie z domyślną implementacją `serde` dla `std::time::Duration`.
- `PathBuf` serializuje się jako ciąg znaków (ścieżka absolutna lub względna względem serwera).
- `Option<T>` pomijane są w JSON, chyba że wartość to `Some(None)` – wówczas serializowane jest `null` (przykład: `OverrideTurnContext.effort: null` czyści ustawienie).
- Bufory bajtów (`Vec<u8>`) w strumieniach (np. `ExecCommandOutputDeltaEvent.chunk`) są kodowane w base64 (`serde_with::base64`).

## Typy bazowe i enumy współdzielone
### Reasoning i verbosity (`config_types.rs`)
| Enum                | Wartość JSON  | Znaczenie                                 |
|---------------------|---------------|-------------------------------------------|
| `ReasoningEffort`   | `minimal`     | Minimalna liczba kroków reasoning         |
|                     | `low`         | Niska intensywność                        |
|                     | `medium`      | Domyślna (wartość `default`)              |
|                     | `high`        | Najwyższe wysiłki reasoning               |
| `ReasoningSummary`  | `auto`        | Automatyczny dobór                        |
|                     | `concise`     | Krótka synteza                            |
|                     | `detailed`    | Szczegółowa synteza                       |
|                     | `none`        | Brak podsumowań reasoning                 |
| `Verbosity`         | `low`         | Najkrótsze odpowiedzi                     |
|                     | `medium`      | Domyślna długość                          |
|                     | `high`        | Maksymalnie rozbudowane odpowiedzi        |
| `SandboxMode`       | `read-only`   | Dostęp tylko do odczytu                   |
|                     | `workspace-write` | Odczyt globalny, zapis w workspace    |
|                     | `danger-full-access` | Pełny dostęp do dysku i sieci      |

### Polityka zgód (`AskForApproval`)
Serde `rename_all = "kebab-case"`; wartości tekstowe:
- `untrusted` (UnlessTrusted) – automatycznie zatwierdzane tylko bezpieczne komendy odczytu.
- `on-failure` – wszystko autozatwierdzane w sandboxie; w razie błędu wymagane ręczne zatwierdzenie poza sandboxem.
- `on-request` – decyzję o proszeniu o zgodę pozostawia się modelowi (domyślna).
- `never` – nigdy nie prosić użytkownika; błędy są zwracane do modelu.

### Polityka sandboxa (`SandboxPolicy`)
Serializacja z polem `mode`:
```json
{"mode": "danger-full-access"}
```
```json
{"mode": "read-only"}
```
```json
{
  "mode": "workspace-write",
  "writable_roots": ["/absolute/path"],
  "network_access": false,
  "exclude_tmpdir_env_var": false,
  "exclude_slash_tmp": false
}
```
- Pole `writable_roots` rozszerza listę katalogów zapisu (poza `cwd`, `/tmp`, `$TMPDIR`).
- `network_access` = `true` dopuszcza ruch sieciowy wewnątrz sandboxa.
- `exclude_tmpdir_env_var` / `exclude_slash_tmp` pozwalają usunąć katalogi tymczasowe z listy domyślnej.
- `WritableRoot { root, read_only_subpaths }` służy do kontroli zapisu (np. `.git`).

### Elementy wejściowe (`InputItem`)
`#[serde(tag = "type", rename_all = "snake_case")]`:
- `{"type":"text", "text":"..."}`
- `{"type":"image", "image_url":"data:image/png;base64,..."}`
- `{"type":"local_image", "path":"/absolute/path.png"}` – serwer konwertuje plik na wariant `image`.

## Submission Queue
### Format przesyłki
```json
{
  "id": "unique-turn-id",
  "op": { ... }  // jedna z operacji Op
}
```
Pole `op` zawiera jedną z operacji poniżej. Nazwy `type` są w `snake_case`.

### Operacje `Op`
#### `interrupt`
Brak dodatkowych pól. Sygnał przerwania bieżącej tury; spodziewaj się `EventMsg::TurnAborted`.

#### `user_input`
```json
{
  "type": "user_input",
  "items": [InputItem, ...]
}
```
Wysyła pojedynczą wiadomość użytkownika bez zmiany kontekstu tury.

#### `user_turn`
Pełna tura rozmowy z kontekstem wykonawczym:
```json
{
  "type": "user_turn",
  "items": [InputItem, ...],
  "cwd": "/Users/alice/project",
  "approval_policy": "on-request",
  "sandbox_policy": {"mode": "workspace-write", "network_access": false},
  "model": "o4-mini",
  "effort": "medium",           // opcjonalne, gdy model wspiera reasoning
  "summary": "auto"              // wymagane, enum ReasoningSummary
}
```

#### `override_turn_context`
Aktualizuje domyślne parametry kolejnych tur:
```json
{
  "type": "override_turn_context",
  "cwd": "/nowa/sciezka",                  // opcjonalne
  "approval_policy": "never",              // opcjonalne
  "sandbox_policy": { ... },
  "model": "o4-mini",
  "effort": null,                           // null usuwa ustawienie reasoning effort
  "summary": "concise"                      // opcjonalne
}
```

#### `exec_approval`
```json
{
  "type": "exec_approval",
  "id": "exec-call-id",
  "decision": "approved" | "approved_for_session" | "denied" | "abort"
}
```
`ReviewDecision` opisuje reakcję użytkownika na prośbę `ExecApprovalRequest`.

#### `patch_approval`
Analogiczne do powyższego, ale dla `ApplyPatchApprovalRequest`.

#### `add_to_history`
```json
{"type":"add_to_history", "text":"Notatka"}
```

#### `get_history_entry_request`
```json
{
  "type": "get_history_entry_request",
  "offset": 0,
  "log_id": 123456
}
```
Prosi o pojedynczy wpis historii globalnej.

#### `get_path`
Zwraca ścieżkę do pliku rollout bieżącej sesji (`EventMsg::ConversationPath`).

#### `list_mcp_tools`
Żąda listy narzędzi MCP dostępnych w sesji (`EventMsg::McpListToolsResponse`).

#### `list_custom_prompts`
Żąda katalogu promptów (`EventMsg::ListCustomPromptsResponse`).

#### `compact`
Prośba o streszczenie kontekstu konwersacji przez model.

#### `review`
```json
{
  "type": "review",
  "review_request": {
    "prompt": "Przeanalizuj zmiany...",
    "user_facing_hint": "Skup się na wydajności"
  }
}
```
Uruchamia podrzędną sesję code review.

#### `shutdown`
Sygnał zamknięcia instancji Codexa. Odpowiedź: `EventMsg::ShutdownComplete`.

## Event Queue
### Format zdarzenia
```json
{
  "id": "submission-id",
  "msg": {
    "type": "agent_message",
    ... payload ...
  }
}
```

### Typy `EventMsg`
Poniższa lista zawiera wszystkie warianty (wartość pola `type` → payload):

**Obsługa błędów i statusu**
- `error` → `{ "message": String }`
- `task_started` → `{ "model_context_window": u64? }`
- `task_complete` → `{ "last_agent_message": String? }`
- `token_count` → `{ "info": TokenUsageInfo? }`
- `turn_aborted` → `{ "reason": "interrupted" | "replaced" }`
- `shutdown_complete` → brak pól (pusty obiekt)
- `stream_error` → `{ "message": String }`
- `background_event` → `{ "message": String }`

**Wiadomości tekstowe i reasoning**
- `agent_message` → `{ "message": String }`
- `agent_message_delta` → `{ "delta": String }`
- `user_message` → `{ "message": String, "kind"?: "plain"|"user_instructions"|"environment_context", "images"?: [String] }`
- `agent_reasoning` → `{ "text": String }`
- `agent_reasoning_delta` → `{ "delta": String }`
- `agent_reasoning_raw_content` → `{ "text": String }`
- `agent_reasoning_raw_content_delta` → `{ "delta": String }`
- `agent_reasoning_section_break` → `{}` (marker rozpoczęcia nowego segmentu reasoning)
- `plan_update` → patrz [Plan tool](#plan-tool-updateplanargs)

**Konfiguracja i historia**
- `session_configured` → `{ "session_id": ConversationId, "model": String, "reasoning_effort"?: ReasoningEffort, "history_log_id": u64, "history_entry_count": usize, "initial_messages"?: [EventMsg], "rollout_path": PathBuf }`
- `conversation_path` → `{ "conversation_id": ConversationId, "path": PathBuf }`
- `get_history_entry_response` → `{ "offset": usize, "log_id": u64, "entry"?: HistoryEntry }`

**Zdarzenia MCP / narzędziowe**
- `mcp_tool_call_begin` → `{ "call_id": String, "invocation": McpInvocation }`
- `mcp_tool_call_end` → `{ "call_id": String, "invocation": McpInvocation, "duration": Duration, "result": CallToolResult | String }`
- `mcp_list_tools_response` → `{ "tools": { fullyQualifiedName: Tool } }`
- `list_custom_prompts_response` → `{ "custom_prompts": [CustomPrompt] }`

**Web search**
- `web_search_begin` → `{ "call_id": String }`
- `web_search_end` → `{ "call_id": String, "query": String }`

**Komendy lokalne / shell**
- `exec_command_begin` → `{ "call_id": String, "command": [String], "cwd": PathBuf, "parsed_cmd": [ParsedCommand] }`
- `exec_command_output_delta` → `{ "call_id": String, "stream": "stdout"|"stderr", "chunk": Base64 }`
- `exec_command_end` → `{ "call_id": String, "stdout": String, "stderr": String, "aggregated_output": String, "exit_code": i32, "duration": Duration, "formatted_output": String }`
- `exec_approval_request` → `{ "call_id": String, "command": [String], "cwd": PathBuf, "reason"?: String }`
- `patch_apply_begin` → `{ "call_id": String, "auto_approved": bool, "changes": { path: FileChange } }`
- `patch_apply_end` → `{ "call_id": String, "stdout": String, "stderr": String, "success": bool }`
- `apply_patch_approval_request` → `{ "call_id": String, "changes": { path: FileChange }, "reason"?: String, "grant_root"?: PathBuf }`
- `turn_diff` → `{ "unified_diff": String }`

**Tryb review**
- `entered_review_mode` → `ReviewRequest`
- `exited_review_mode` → `{ "review_output"?: ReviewOutputEvent }`

## Struktury powiązane ze zdarzeniami
### TokenUsage i TokenUsageInfo
```json
{
  "total_token_usage": {
    "input_tokens": 123,
    "cached_input_tokens": 45,
    "output_tokens": 67,
    "reasoning_output_tokens": 10,
    "total_tokens": 245
  },
  "last_token_usage": { ... },
  "model_context_window": 128000
}
```
- `TokenUsage.blended_total()` = `non_cached_input + output_tokens`.
- Konstanta bazowa `BASELINE_TOKENS = 12000` służy do szacowania zajętości kontekstu.

### ParsedCommand
Wartości `type`: `read`, `list_files`, `search`, `unknown`. Pola:
- `read { cmd: String, name: String }`
- `list_files { cmd: String, path?: String }`
- `search { cmd: String, query?: String, path?: String }`
- `unknown { cmd: String }`

### McpInvocation i CallToolResult
```json
{
  "server": "todo",
  "tool": "plan/update",
  "arguments": { ... } | null
}
```
`CallToolResult` pochodzi z `mcp_types` i odzwierciedla wynik MCP (sukces/błąd).

### Zmiany plików (`FileChange`)
```json
{"type": "add", "content": "..."}
{"type": "delete", "content": "..."}
{"type": "update", "unified_diff": "@@ ...", "move_path": "/new/path.rs"}
```

### Review
- `ReviewRequest { "prompt": String, "user_facing_hint": String }`
- `ReviewOutputEvent` zawiera:
  - `findings: [ReviewFinding]`
  - `overall_correctness: String`
  - `overall_explanation: String`
  - `overall_confidence_score: f32`
- `ReviewFinding { title, body, confidence_score, priority, code_location }`
- `ReviewCodeLocation { absolute_file_path: PathBuf, line_range: { start: u32, end: u32 } }`

### Plan tool (`UpdatePlanArgs`)
- `UpdatePlanArgs { explanation?: String, plan: [PlanItemArg] }`
- `PlanItemArg { step: String, status: "pending" | "in_progress" | "completed" }`

## Struktury odpowiedzi modelu (`models.rs`)
### `ResponseInputItem`
- `message { role: String, content: [ContentItem] }`
- `function_call_output { call_id: String, output: FunctionCallOutputPayload }`
- `mcp_tool_call_output { call_id: String, result: Result<CallToolResult,String> }`
- `custom_tool_call_output { call_id: String, output: String }`

### `ResponseItem`
- `message { id?: String, role: String, content: [ContentItem] }`
- `reasoning { id: String, summary: [ReasoningItemReasoningSummary], content?: [ReasoningItemContent], encrypted_content?: String }`
- `local_shell_call { id?: String, call_id?: String, status: "completed"|"in_progress"|"incomplete", action: LocalShellAction }`
- `function_call { id?: String, name: String, arguments: String, call_id: String }`
- `function_call_output { call_id: String, output: FunctionCallOutputPayload }`
- `custom_tool_call { id?: String, status?: String, call_id: String, name: String, input: String }`
- `custom_tool_call_output { call_id: String, output: String }`
- `web_search_call { id?: String, status?: String, action: WebSearchAction }`
- `other` – dane nieobsługiwane.

`ContentItem` typy: `input_text`, `input_image`, `output_text`. `ReasoningItemContent`: `reasoning_text`, `text`.

`FunctionCallOutputPayload` serializuje się jako zwykły string (`content`). `success` służy tylko lokalnie.

### Local shell
- `LocalShellAction::Exec { command: [String], timeout_ms?: u64, working_directory?: String, env?: {k:v}, user?: String }`
- `LocalShellStatus`: `completed`, `in_progress`, `incomplete`.

### Web search
- `WebSearchAction::Search { query: String }`

### Konwersja `InputItem` → `ResponseInputItem`
`Vec<InputItem>` (np. w `ResponseInputItem::from`) jest zamieniane na wiadomość `role=user` z odpowiednimi `ContentItem`. Pliki lokalne zamieniają się w data URL (MIME wyznaczane przez `mime_guess`).

## Historia i rollouty
- `InitialHistory`: `new`, `resumed { conversation_id, history: [RolloutItem], rollout_path }`, `forked([RolloutItem])`.
- `ResumedHistory { conversation_id, history: [RolloutItem], rollout_path }`.
- `RolloutItem` typy:
  - `session_meta(SessionMetaLine)`
  - `response_item(ResponseItem)`
  - `compacted(CompactedItem)`
  - `turn_context(TurnContextItem)`
  - `event_msg(EventMsg)`
- `RolloutLine { timestamp: String, item: RolloutItem }`.
- `SessionMeta { id: ConversationId, timestamp: String (RFC3339), cwd: PathBuf, originator: String, cli_version: String, instructions?: String }`.
- `SessionMetaLine { meta: SessionMeta, git?: GitInfo }`.
- `GitInfo { commit_hash?, branch?, repository_url? }`.
- `TurnContextItem { cwd: PathBuf, approval_policy: AskForApproval, sandbox_policy: SandboxPolicy, model: String, effort?: ReasoningEffort, summary: ReasoningSummary }`.
- `CompactedItem { message: String }` może być traktowany jak `ResponseItem::Message` z rolą `assistant`.

`HistoryEntry { conversation_id: String, ts: u64, text: String }` zwracane w `GetHistoryEntryResponse`.

## Plan tool (`UpdatePlanArgs`)
Patrz sekcja [Plan tool](#plan-tool-updateplanargs). Umożliwia spójne aktualizowanie kroków planu przez `EventMsg::PlanUpdate`.

## Protokół MCP (JSON-RPC)
### Transport
- MCP używa JSON-RPC 2.0 z metodami `camelCase`.
- Każdy request przyjmuje pole `id` (`RequestId` – liczba całkowita lub string) i `params` (jeśli wymagane).
- Odpowiedzi (`*Response`) mają taki sam `id` i zwykłą strukturę JSON-RPC (`result` / `error`).
- Serwer może wysyłać:
  - **ServerRequest** (metody `applyPatchApproval`, `execCommandApproval`) – wymagają odpowiedzi klienta.
  - **ServerNotification** (`authStatusChange`, `loginChatGptComplete`) – jednostronne powiadomienia bez `id`.

### Requests klienta (`ClientRequest`)
| Metoda | Parametry | Wynik |
|--------|-----------|-------|
| `newConversation` | `NewConversationParams { model?, profile?, cwd?, approval_policy?, sandbox?, config?, base_instructions?, include_plan_tool?, include_apply_patch_tool? }` | `NewConversationResponse { conversation_id, model, reasoning_effort?, rollout_path }`
| `listConversations` | `ListConversationsParams { page_size?, cursor? }` | `ListConversationsResponse { items: [ConversationSummary], next_cursor? }`
| `resumeConversation` | `ResumeConversationParams { path, overrides? }` | `ResumeConversationResponse { conversation_id, model, initial_messages? }`
| `archiveConversation` | `ArchiveConversationParams { conversation_id, rollout_path }` | `ArchiveConversationResponse {}`
| `sendUserMessage` | `SendUserMessageParams { conversation_id, items }` | `SendUserMessageResponse {}`
| `sendUserTurn` | `SendUserTurnParams { conversation_id, items, cwd, approval_policy, sandbox_policy, model, effort?, summary }` | `SendUserTurnResponse {}`
| `interruptConversation` | `InterruptConversationParams { conversation_id }` | `InterruptConversationResponse { abort_reason }`
| `addConversationListener` | `AddConversationListenerParams { conversation_id }` | `AddConversationSubscriptionResponse { subscription_id }`
| `removeConversationListener` | `RemoveConversationListenerParams { subscription_id }` | `RemoveConversationListenerResponse {}`
| `gitDiffToRemote` | `GitDiffToRemoteParams { cwd }` | `GitDiffToRemoteResponse { sha, diff }`
| `loginApiKey` | `LoginApiKeyParams { api_key }` | `LoginApiKeyResponse {}`
| `loginChatGpt` | brak | `LoginChatGptResponse { login_id, auth_url }`
| `cancelLoginChatGpt` | `CancelLoginChatGptParams { login_id }` | `CancelLoginChatGptResponse {}`
| `logoutChatGpt` | brak | `LogoutChatGptResponse {}`
| `getAuthStatus` | `GetAuthStatusParams { include_token?, refresh_token? }` | `GetAuthStatusResponse { auth_method?, auth_token?, requires_openai_auth? }`
| `getUserSavedConfig` | brak | `GetUserSavedConfigResponse { config: UserSavedConfig }`
| `setDefaultModel` | `SetDefaultModelParams { model?, reasoning_effort? }` | `SetDefaultModelResponse {}`
| `getUserAgent` | brak | `GetUserAgentResponse { user_agent }`
| `userInfo` | brak | `UserInfoResponse { alleged_user_email? }`
| `execOneOffCommand` | `ExecOneOffCommandParams { command, timeout_ms?, cwd?, sandbox_policy? }` | `ExecArbitraryCommandResponse { exit_code, stdout, stderr }`

### ServerRequest (żądania od serwera)
- `applyPatchApproval` → `ApplyPatchApprovalParams { conversation_id, call_id, file_changes, reason?, grant_root? }` – klient odpowiada `ApplyPatchApprovalResponse { decision }`.
- `execCommandApproval` → `ExecCommandApprovalParams { conversation_id, call_id, command, cwd, reason? }` – klient odpowiada `ExecCommandApprovalResponse { decision }`.

### ServerNotification
- `authStatusChange` → `{ "auth_method"?: "apiKey" | "chatGpt" }`
- `loginChatGptComplete` → `{ "login_id": Uuid, "success": bool, "error"?: String }`

### Konfiguracja użytkownika (`UserSavedConfig`)
- Pola opcjonalne opisują preferowane ustawienia (policy, sandbox, model, profile, narzędzia).
- `profiles` to mapa `profil -> Profile { model?, model_provider?, approval_policy?, model_reasoning_*?, chatgpt_base_url? }`.
- `tools`: `web_search?`, `view_image?` (bool).
- `sandbox_settings`: jak w `SandboxPolicy::WorkspaceWrite`.

### Typy wspólne MCP ↔ protokół
- MCP używa tej samej definicji `InputItem`, `AskForApproval`, `SandboxPolicy`, `ReviewDecision`, `FileChange`, co kolejki SQ/EQ.
- Wszystkie pola `camelCase` w MCP odpowiadają polom `snake_case` w wewnętrznym protokole.

### Przykładowa sekwencja MCP
1. Klient wysyła `newConversation` z ustalonym modelem.
2. Po otrzymaniu `conversation_id` wysyła `addConversationListener` aby zasubskrybować zdarzenia.
3. Wysyła `sendUserTurn` (z tym samym `conversation_id`).
4. Serwer strumieniuje zdarzenia (`EventMsg` zakodowane np. jako JSON Lines lub SSE – implementacja transportu zależy od warstwy klienckiej) oraz ewentualne `ServerRequest` (zatwierdzenia).
5. Klient odpowiada `applyPatchApproval` / `execCommandApproval` zgodnie z decyzją użytkownika.
6. Po zakończeniu pracy – opcjonalne `interruptConversation` lub `archiveConversation`.

## Przebieg tury (SQ/EQ)
1. **Submission**: klient publikuje `Submission` (`user_turn` lub `user_input`).
2. **Echo input**: agent wysyła `EventMsg::UserMessage` (co zostało przekazane modelowi) oraz `SessionConfigured` przy starcie.
3. **Stream reasoning**: `AgentMessageDelta`, `AgentReasoningDelta`, `TokenCount` itd. pojawiają się asynchronicznie.
4. **Narzędzia**: gdy model uruchamia polecenia lub narzędzia MCP, pojawiają się `ExecCommand*`, `McpToolCall*`, `WebSearch*`, `PlanUpdate`.
5. **Zgody**: jeżeli konieczna zgoda użytkownika, agent wysyła `ExecApprovalRequest` / `ApplyPatchApprovalRequest`. Klient musi odpowiedzieć `exec_approval` / `patch_approval`.
6. **Zakończenie**: `TaskComplete` + opcjonalnie `TurnDiff`, `TokenCount`. Błąd powoduje `Error` / `TurnAborted`.
7. **Historia**: klient może zainicjować `get_path` lub `get_history_entry_request` aby odczytać logi.

## Uwagi implementacyjne
- Warto używać UUID-ów (lub monotonie rosnących stringów) jako `Submission.id` aby uniknąć kolizji.
- Obsługuj wiele zdarzeń dla jednego `id` – `TaskStarted`, `AgentMessageDelta`, `TaskComplete` będą miały ten sam identyfikator.
- Przy dekodowaniu `Result<CallToolResult, String>` (np. w `McpToolCallEndEvent.result`) pamiętaj, że w JSON wynik może być obiektem narzędzia lub stringiem błędu.
- `ExecCommandOutputDeltaEvent.chunk` należy dekodować z base64 zanim pokaże się użytkownikowi.
- `SessionConfigured.initial_messages` może zawierać listę pełnych `EventMsg`; traktuj je tak samo jak normalne zdarzenia.
- `OverrideTurnContext.effort` przyjmuje trzy stany: brak (bez zmian), `null` (wyczyść), jedna z wartości `ReasoningEffort` (ustaw).
- Przy integracji MCP pamiętaj o utrzymywaniu subskrypcji (`removeConversationListener`) oraz obsłudze wielokrotnych rozmów równocześnie.
- Rolling log (`rollout_path`) jest plikiem JSONL – każdy wiersz to `RolloutLine`.
- Gdy sandbox ma `workspace_write`, serwer automatycznie dodaje bieżące `cwd`, `/tmp` (jeśli istnieje) i `$TMPDIR` (chyba że wyłączone flagami) do listy zapisu.

## Podsumowanie
Dokument ten definiuje wszystkie struktury danych, enumeracje i przebiegi wymagane do zbudowania klienta, który komunikuje się z Codexem przez kolejki `Submission`/`Event` lub interfejs MCP. Implementacja powinna ściśle przestrzegać przedstawionych nazw pól, wartości `type` oraz formatów JSON, aby zachować kompatybilność z istniejącymi komponentami Codexa.
