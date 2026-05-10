---
layout: default
title: Agent Activity Ledger and Coordination Graph
---

# Agent Activity Ledger and Coordination Graph

**Status:** Research / Future Roadmap<br>
**Priority:** Medium-high once core agent/session plumbing is stable<br>
**Tracking issue:** [#147](https://github.com/bglusman/calciforge/issues/147)<br>
**Related:** `docs/roadmap/agent-recipes-orchestrators.md`, `docs/roadmap/product-ux.md`, `docs/roadmap/team-chatops-slack-discord.md`

## Idea

Calciforge could become the shared nervous system for a fleet of agents: not
just a chat router or policy gateway, but a structured, searchable, cross-agent
record of what is happening, why it is happening, and how agents should
coordinate.

The motivating thread is that logs are too flat and prompt-only decision
tracking is too cooperative. Agents need system-wide situational awareness, but
operators also need trustworthy evidence about what actually happened.

## Goals

- Give agents visibility into current system activity without dumping raw logs
  into their context.
- Record highly structured metadata for agent activity, decisions, actions,
  artifacts, outcomes, blockers, and handoffs.
- Make the activity graph searchable, indexable, replayable, and useful to both
  humans and agents.
- Support flexible multi-agent coordination patterns such as claims, leases,
  handoffs, dependencies, subscriptions, reviews, and structured questions.
- Preserve a trust boundary between agent-supplied rationale and
  Calciforge-observed ground truth.

## Non-Goals

- Do not replace application logs, tracing, or metrics. This layer should
  summarize and relate operational facts, not become a second unstructured log
  sink.
- Do not require every upstream agent to faithfully self-report in order to be
  useful.
- Do not expose private operator context broadly just because events are
  searchable. Visibility needs identity, project, channel, and agent scoping.

## Core Shape

A useful first abstraction is an append-only activity ledger with typed events
and stable IDs. Events can then be projected into a coordination graph.

```text
Actor -> Intent -> Decision -> Action -> Artifact -> Outcome
                  ↘ blocks / supersedes / depends_on / reviews / claims
```

Example event families:

- `intent.captured` — user request, normalized task, source channel/session
- `task.started`, `task.blocked`, `task.completed`, `task.cancelled`
- `decision.proposed`, `decision.accepted`, `decision.superseded`
- `tool.called`, `tool.allowed`, `tool.denied`, `tool.completed`
- `file.changed`, `diff.produced`, `commit.created`, `artifact.attached`
- `test.started`, `test.failed`, `test.passed`
- `handoff.requested`, `handoff.accepted`, `review.requested`
- `lease.claimed`, `lease.released`, `dependency.added`
- `observation.recorded`, `risk.flagged`, `policy.verdict_recorded`

Each record should include at least:

- event id and correlation id
- actor identity: user, agent, runtime, tool, or system component
- project/repo/session/channel scope
- timestamp and monotonic ordering where available
- event type and versioned schema
- related entity IDs
- privacy/security labels
- evidence pointers rather than giant payloads by default

## Agent-Supplied Meaning vs Observed Facts

The system should not depend on agents politely logging the truth. Calciforge and
its adapters should record facts they can observe:

- which tool was called
- which command was run
- which files changed
- which tests ran
- which policy decision was returned
- which artifacts were created
- which chat/session message initiated the work

Agents can enrich that record with rationale:

- why they chose an approach
- alternatives they considered
- confidence level
- expected outcome
- what would make them revisit the decision

A reconciliation layer can compare the two:

- Does the stated intent match the files actually touched?
- Did an agent edit code without a linked task or action?
- Did a decision claim tests passed when the observed test event failed?
- Did two agents claim the same file range or task at once?

This is the blend suggested by Deciduous-style decision graphs, Beads-style work
tracking, and Calciforge's policy/tool boundary: semantic memory layered on top
of independently captured operational evidence.

## Query and Visibility Examples

Agents and operators should be able to ask:

- What is happening right now across Calciforge?
- Who is working on this repository, task, or file?
- What changed since my last session?
- Why did another agent choose this approach?
- Which tasks are blocked, and on whom or what?
- What recent failures are relevant to this file?
- What decisions were superseded by the current implementation?
- Which outbound messages, tool calls, or policy denials are related to this
  task?

Useful access modes:

- timeline replay
- graph traversal by task/decision/artifact
- full-text search
- vector search over summaries/rationale
- compact per-session summaries
- live watch/subscription views
- operator dashboard cards

## Coordination Layer

Once typed activity exists, coordination can be structured instead of ad hoc
agent chat.

Possible primitives:

- **Claims/leases:** an agent claims a task, repo, file, or resource for a
  bounded time.
- **Handoffs:** one agent transfers context, artifacts, blockers, and next steps
  to another.
- **Subscriptions:** an agent watches a task, file, issue, policy area, or
  failing test.
- **Structured questions:** agents ask each other or the operator questions with
  typed answer options and deadlines.
- **Review requests:** an agent requests review for a decision, diff, security
  exception, or final answer.
- **Conflict detection:** Calciforge flags overlapping edits, duplicate work,
  or contradictory decisions.

These primitives should be available over local APIs/MCP/ACP where possible so
multi-agent systems can coordinate without scraping chat transcripts.

## Product Surface

Potential user-facing surfaces:

- `!status` can summarize active tasks, blocked agents, pending approvals, and
  recent important outcomes.
- A local web control surface can show the live activity graph, task claims,
  decision chains, and policy history.
- Agent session recovery can ask the ledger for the minimum relevant context
  instead of relying only on a transcript summary.
- GitHub issues/PRs can link to activity chains and decision writeups.
- Roadmap and design docs can seed tasks into the work graph before a PR exists.

## Open Questions

- What is the minimal event schema that is useful without becoming a tracing
  platform?
- Should the ledger be backed by SQLite, event JSONL, OpenTelemetry-compatible
  spans, or a hybrid?
- Which events are safe to expose to which agents by default?
- How should retention, redaction, and compaction work for sensitive events?
- Is this one Calciforge subsystem or a separate crate/service that Calciforge
  embeds?
- Should existing tools such as Deciduous, Beads, or Rust Beads be integrated,
  imported from, or treated mainly as inspiration?
- How do we prevent the coordination graph from becoming another noisy queue
  agents ignore?

## First Research Slice

1. Define a small versioned `ActivityEvent` schema and write it as JSONL in a
   Calciforge-owned directory.
2. Emit events for inbound messages, agent dispatch, tool/policy decisions,
   artifacts, and final responses.
3. Add a read-only query command/API for recent activity by project, session,
   and actor.
4. Add a tiny projection that groups events into task/activity chains using
   correlation IDs.
5. Test one agent recovery workflow: "summarize what happened since my last
   turn" from the ledger instead of from raw logs.
6. Only after that, evaluate richer graph storage and coordination primitives.
