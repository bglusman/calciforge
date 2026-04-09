# ZeroClawed Roadmap

## Completed
- [x] Policy enforcement via clashd sidecar and OpenClaw plugin
- [x] Mutual editing rules (librarian ↔ custodian approval required)
- [x] 148 clippy warnings fixed
- [x] 374 tests passing
- [x] OpenClaw >=2026.3.24-beta.2 compatibility
- [x] HOOK.md format fixed with proper YAML frontmatter
- [x] Plugin installation and verification
- [x] clangd sidecar deployment and health checks

## In Progress
- [ ] Outpost domain filtering enhancement (allowlist/blocklist per agent)
- [ ] CI lint/format checks passing (current PR)
- [ ] Documentation updates for policy enforcement

## Backlog / Research Phase

### Outpost Domain Filtering
- Explicit domain allowlist/blocklist controls
- Per-agent policy profiles with shared defaults
- Integration with clash/starlark for unified policy
- Coordinated enforcement between outpost and clashd
- Audit logging for domain decisions

## Future Considerations
- Agent-specific policy profiles in clashd
- Shared policy templates with overrides
- Enhanced outpost/clash coordination for network policy
- Web dashboard for policy monitoring and audit logs
- Integration with additional LLM providers and tools

---
*Last updated: 2026-04-09*
