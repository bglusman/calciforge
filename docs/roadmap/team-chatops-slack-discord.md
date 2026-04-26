# Team ChatOps: Slack, Discord, and Castle Federation

**Status:** sketch
**Priority:** medium, after single-operator flows stay boringly reliable
**Related:** `docs/rfcs/model-gateway-primitives.md`, `docs/architecture-review-2026-04-25.md`

Calciforge works today as a personal, cross-channel gateway: one operator, many chat surfaces, several agents, one security boundary. Team ChatOps adds shared rooms, shared agents, and more people near the door. That changes the risk model.

## Goals

- Add Slack and Discord adapters without weakening the existing Telegram, Matrix, WhatsApp, and Signal identity model.
- Keep per-user identity, audit, command authorization, and active-agent state even when multiple people share a room.
- Make secret access user-scoped by default: an agent may know a secret name, but resolution should still depend on the requesting identity and destination.
- Prepare for "Doors to other Castles": federation between Calciforge instances where one household can talk to another under explicit trust rules.

## Non-Goals

- No public multi-tenant SaaS assumptions.
- No shared room where every participant automatically inherits owner permissions.
- No plaintext secret readback, even for administrators.
- No bot-admin-only security model. Chat platform admin rights are not the same as Calciforge authorization.

## Slack and Discord Shape

Each adapter should produce the same internal envelope:

```text
channel_kind
channel_workspace_id
channel_room_id
channel_thread_id
sender_platform_id
resolved_identity
message_text
attachments
```

The important difference from one-to-one channels is that room and thread identity become first-class routing inputs. A user can have one default agent in direct messages and a different default agent in a team room. Commands that change shared state should say which scope they are changing.

## Authorization Defaults

- Direct message: preserve today's per-identity defaults.
- Private room: require explicit room allowlist before routing agent messages.
- Public room: default to observe-only or command-only until configured.
- Shared agent: require per-agent allowed identities and per-room allowed commands.
- Secret substitution: require both identity and destination checks.

## Federation Sketch

"Doors to other Castles" means a Calciforge instance can expose a narrow endpoint to another trusted Calciforge instance. The receiving Castle should see:

- the sending Castle identity
- the local user identity, if intentionally forwarded
- the requested agent/tool/scope
- the policy decision envelope
- an audit correlation id

Federation should be built on the same `DecisionContext` envelope recommended by the architecture review. Without that shared envelope, federation risks becoming an ad hoc pile of special cases.

## Open Questions

- Should room-level routing be a separate config table or an extension of `[[routing]]`?
- What is the smallest useful Slack/Discord MVP: direct messages only, private rooms, or both?
- How should a team room ask for a one-shot secret paste without exposing the paste URL to everyone in the room?
- Do federated Castles trust each other's identity assertions directly, or should every remote identity map to a local shadow identity first?

## First Implementation Slice

1. Add shared room/thread fields to the internal message envelope.
2. Add config validation for room allowlists and room-scoped default agents.
3. Implement one adapter first, probably Discord because local bot testing is lighter.
4. Add audit entries that distinguish direct-message actions from room-scoped actions.
5. Only then add federation endpoints.
