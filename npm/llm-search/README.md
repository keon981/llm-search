# @k/llm-search

Prebuilt binary for the llm-search MCP server (Brave / Tavily routed web search, multi-provider page fetch). No Rust toolchain needed.

```bash
npm install -g @k/llm-search
claude mcp add llm-search -s user -e BRAVE_API_KEY=... -e TAVILY_API_KEY=... -- llm-search
```

See https://github.com/keon981/llm-search for tools and configuration.
