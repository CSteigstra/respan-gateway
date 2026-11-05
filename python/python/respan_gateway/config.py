"""Types for the configuration mapping, for editor completion and checking.

They describe what `Gateway(config=...)` takes (the format of
`gateway.example.toml`). At run time a config is a plain dict, and a mistake
in one is a `ConfigError`.
"""

from __future__ import annotations

from typing import Dict, List, TypedDict, Union


class Auth(TypedDict, total=False):
    """A provider's credentials as one header, or Google tokens the gateway mints."""

    type: str  # "bearer", "header" or "google"
    key: str  # literal or "env:NAME"
    name: str  # the header, for type "header"
    # For type "google": a credentials file's JSON (service account or authorized_user),
    # literal, "env:NAME" or "file:PATH"; the token is minted and refreshed in the gateway.
    credentials: str
    scope: str  # for type "google"; Cloud Platform's if unset


class ProviderConfig(TypedDict, total=False):
    """One provider: a vendor, explicit formats, or a vendor with some settings replaced."""

    vendor: str  # "openai", "anthropic", "gemini", "groq", ...
    # e.g. "openai-chat", ["openai-images", "openai-embeddings"], or each at its own base URL:
    # {"openai-chat": "https://a.example/v1", "anthropic-messages": "https://b.example"}
    formats: Union[str, List[str], Dict[str, str]]
    base_url: str
    key: str  # literal or "env:NAME"
    auth: Auth
    proxy: Union[str, bool]  # its own proxy URL, literal or "env:NAME"; False for none


class OAuthConfig(TypedDict, total=False):
    """The gateway's own OAuth client for an HTTP MCP server (machine to machine)."""

    token_url: str  # required
    client_id: str  # required; literal or "env:NAME"
    client_secret: str  # required; literal or "env:NAME"
    scopes: List[str]
    refresh_token: str  # literal or "env:NAME"; the refresh token grant, else client credentials


class McpAuth(TypedDict, total=False):
    """How the gateway authenticates to an HTTP MCP server: one of the keys.

    `auth` may also be "login" (the login `respan-gateway mcp login <name>`
    saved) or "forward" (each client's own credentials, passed on).
    """

    bearer: str  # literal or "env:NAME", sent as a bearer token
    oauth: OAuthConfig  # a token from a token endpoint


class McpConfig(TypedDict, total=False):
    """One MCP server, served at /mcp/{name}, in the fields of an `mcp.json`:
    an HTTP `url` (with `headers`, `auth`), a stdio `command` (with `args`,
    `env`, `cwd`), or a tool `vendor` (with `key`, `base_url`)."""

    url: str  # a streamable-HTTP endpoint
    headers: Dict[str, str]  # values literal or "env:NAME"; all treated as secrets
    auth: Union[McpAuth, str]  # {"bearer": ..}, {"oauth": {..}}, "login" or "forward"
    command: str  # a stdio server's program alone
    args: List[str]  # its arguments
    env: Dict[str, str]  # values literal or "env:NAME"
    cwd: str
    vendor: str  # a tool vendor's id, such as "linkup" or "tinyfish"
    key: str  # a tool vendor's key, literal or "env:NAME"; its own variable if unset
    base_url: str  # a tool vendor's base URL, if not its own
    proxy: Union[str, bool]  # its own proxy URL, literal or "env:NAME"; False for none


class ToolLoopConfig(TypedDict, total=False):
    """The tool loop: the gateway runs the tools of tool servers inside chat requests."""

    servers: List[str]  # the tool servers whose tools are offered; all but auth "forward" ones if unset
    allow: List[str]  # tool names as the model has them, "github__*"; all if unset
    deny: List[str]  # never offered; wins over `allow`
    models: List[str]  # model prefixes the loop applies to; every chat model if unset
    max_steps: int
    timeout: Union[int, float, str]  # seconds, or "5m"
    tool_timeout: Union[int, float, str]
    tool_list_ttl: Union[int, float, str]  # how long a server's tool list is kept
    parallel: int  # tool calls of one step that run at once
    max_result: Union[int, str]  # bytes, or a size such as "64KiB"
    max_results: Union[int, str]  # bytes of tool answers in one request


class NetworkConfig(TypedDict, total=False):
    """Outbound connections: their proxy, and certificates trusted besides the system's."""

    proxy: Union[str, bool]  # "http://user:password@proxy:3128", literal or "env:NAME"; False for none; HTTPS_PROXY & co. if unset
    no_proxy: List[str]  # hosts reached directly: "localhost", ".corp.internal", "10.0.0.0/8"
    ca: str  # a PEM file of certificate authorities, relative to the config file


class CorsConfig(TypedDict):
    """Browser access: pages on these origins may call the gateway."""

    origins: List[str]  # "https://app.example.com"; "*" needs client_keys


class RoutingConfig(TypedDict, total=False):
    """Which provider each request goes to; its rules are the top-level `routes`."""

    default: str  # the provider of each kind it serves, for requests nothing else routes
    listings: bool  # read each provider's model list to route by it; true if unset


class RouteTarget(TypedDict, total=False):
    """A route's candidate with a weight, for `pick` "weighted"."""

    target: str  # required; "provider", "provider/model" or "provider/{model}"
    weight: int  # 1 if unset


class RouteWhen(TypedDict, total=False):
    """Which requests a route applies to; every request if unset."""

    clients: List[str]  # client keys, by name
    kinds: List[str]  # "chat", "embedding", ...


class RouteConfig(TypedDict, total=False):
    """Where a model name goes, and how its candidates are picked, retried and raced."""

    to: Union[str, RouteTarget, List[Union[str, RouteTarget]]]  # required; "anthropic/claude-opus-4-1"
    pick: str  # "first" (default), "cheapest", "fastest", "weighted" or "random"
    needs: List[str]  # capabilities from the provider's model listing: "tools", "vision"
    when: RouteWhen
    fallback: bool  # try the next candidate when one fails; True if unset
    hedge: Union[int, float, str]  # race the candidates instead, each started this long after the last; "200ms"
    tries: int  # per candidate, retries included; 1 to 10
    backoff: Union[int, float, str]  # the first wait before a retry, doubled after each; "250ms"
    max_backoff: Union[int, float, str]  # the longest wait; "10s"
    until: str  # "head" or "end": how far an answer is read before it counts


class RequestLogConfig(TypedDict, total=False):
    """The request history the web UI and admin API read."""

    history: int  # finished requests kept in memory


class ResponsesConfig(TypedDict, total=False):
    """Conversation state the gateway keeps, in its memory: Responses translated
    to another provider's format, and a Chat client's reasoning between tool
    calls with OpenAI's Responses."""

    enabled: bool  # True if unset
    keep: Union[int, float, str]  # after it was last read; "1h" if unset


class _RespanKey(TypedDict):
    key: str  # literal, or "env:NAME"


class RespanConfig(_RespanKey, total=False):
    """Each request sent to Respan as a trace."""

    url: str  # the OTLP trace endpoint; Respan's if unset
    content: bool  # send a model's messages and reply; True if unset
    service: str  # the service the traces are of; "respan-gateway" if unset


class PluginsConfig(TypedDict, total=False):
    """The compiled-in plugins, each under its name; the gateway orders them."""

    routing: RoutingConfig
    tool_loop: ToolLoopConfig
    responses: ResponsesConfig
    respan: RespanConfig
    request_log: RequestLogConfig


class _Rates(TypedDict):
    input: float
    output: float


class Price(_Rates, total=False):
    """Dollars per million tokens, and per second of audio."""

    cached_input: float  # for input tokens read from a cache; `input` if unset
    cache_write: float  # for input tokens written to a cache; `input` if unset
    per_second: float  # for a model billed by the second of audio


class ModelConfig(TypedDict, total=False):
    """A declared model, laid over what its provider's list says of it."""

    pricing: Price  # a call's cost is its model's pricing
    kind: str
    maker: str  # who made it, e.g. "anthropic", for a provider serving others' models
    display_name: str
    description: str
    context_window: int
    max_output_tokens: int
    input_modalities: List[str]  # "image", "audio", "video", "document"
    capabilities: List[str]  # "tools", "reasoning", "structured_outputs"
    created: str  # RFC 3339 or a date
    retires_at: str  # RFC 3339 or a date


class _NamedKey(TypedDict):
    name: str  # what the request log calls this client
    key: str  # literal or env:NAME


class NamedClientKey(_NamedKey, total=False):
    # Whether the key reaches the admin API (`/ui/api/*`). Unset on every key:
    # all keys do. Once any key sets True, only the keys that do.
    admin: bool
    # The tool loop and aggregate tools (`server__tool` globs) this key's requests
    # may use, within the global allow/deny; all if unset.
    tools: List[str]


class GatewayConfig(TypedDict, total=False):
    """The top level of a gateway configuration."""

    providers: Dict[str, ProviderConfig]
    formats: List[str]  # the formats served to clients
    routes: Dict[str, RouteConfig]  # by the model name asked for, or a glob such as "llama-*"
    client_keys: List[Union[str, NamedClientKey]]  # when set, every request needs one; a bare key is named key-1, key-2...
    plugins: PluginsConfig
    listen: str
    body_limit: Union[int, str]  # bytes, or a size such as "32MiB"
    log: str  # "text", "json" or "off"
    ws_message_limit: Union[int, str]  # bytes, or a size such as "16MiB"
    models: Dict[str, ModelConfig]  # by "provider/model": laid over its provider's list
    shutdown_grace: Union[int, float, str]  # seconds, or "30s", "2m", "500ms"
    tools: Dict[str, McpConfig]  # tool servers by name: an MCP `url` or `command`, or a tool `vendor`
    cors: CorsConfig
    network: NetworkConfig
    ui: bool  # False also turns off the admin API behind models()/requests()/providers()
