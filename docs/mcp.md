# MCP tools

Use `ion mcp add` for a local stdio server or `ion mcp add-http` for a remote
Streamable HTTP server. `ion mcp list` shows saved configuration; `/mcp` in chat
shows connection status and tool counts.

Enabled servers connect in parallel without blocking chat from opening. Model
requests wait for connecting servers configured to expose direct tools. Deferred
servers don't block ordinary requests: `tool_search` waits for their inventory
when called and loads matching schemas for the next request. Use `/mcp` to check
if a server is still connecting or why it failed. Both exposure modes respect the
[selected tool loadout](tool-control.md#select-the-models-tools).

## Use Context7 for library documentation

Export `CONTEXT7_API_KEY` in the environment that will launch Ion. Then register
the [Context7 HTTP endpoint](https://context7.com/docs/resources/all-clients):

```sh
ion mcp add-http context7 https://mcp.context7.com/mcp \
  --bearer-token-env CONTEXT7_API_KEY \
  --exposure deferred \
  --description 'Context7 library documentation and code examples'
```

Ion stores the variable name, not its value, and sends the key as a bearer token
to the MCP server. Start a new Ion process after changing the configuration or
the exported key. Check `/mcp`, then try a prompt such as:

> Use Context7 to check how Cargo resolves a workspace lockfile.

Ion makes the server's `resolve-library-id` and `query-docs` tools available
through `tool_search`. If you know the Context7 library ID, include it in your
prompt to skip resolution. For automatic use, add a rule to your Ion home's
`AGENTS.md` telling the model to use `tool_search` for Context7 when checking
library or API documentation. Send focused questions, not source code,
credentials or other private data.

`ion mcp disable context7` stops new clients from connecting without deleting
the configuration. `ion mcp enable context7` restores it. To reconnect a server
already configured in the active process, use `/mcp reconnect context7`.
