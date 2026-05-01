---
layout: default
title: Channel-Native UI
---

# Channel-Native UI

Calciforge keeps text commands as the portable interface, then adds native
controls when a channel can render them reliably. A button press, list choice,
approval decision, or form link must call the same backend command handler as
the text command so the behavior stays identical across direct clients,
bridged clients, and text-only channels.

The channel with the best controls does not have to be the channel where the
conversation happens. For example, an operator can keep Telegram open as a
Calciforge control panel for `!agent`, `!model`, `!session`, approval, and
`!secret` flows while chatting with the same routed agent over Matrix,
WhatsApp, Signal, or SMS. Active agent, model, and session selections are keyed
by Calciforge identity, so the operator's choices follow them across channels.

Use `ui_mode = "auto"` to allow native controls where supported. Use
`ui_mode = "text"` when a client or bridge renders native controls poorly, such
as WhatsApp through a Matrix bridge.

## Rendered Mockups and Targets

These illustrations are rendered mockups for docs and planning. They show the
intended user experience and the current implementation boundary; they are not
captured screenshots from Telegram, Matrix, Signal, or WhatsApp clients unless
explicitly labeled that way.

<div class="channel-ui-grid">
  <figure>
    <img src="assets/channel-ui-telegram-agent.svg" alt="Telegram agent selection with inline buttons">
    <figcaption>Telegram can render agent choices as inline buttons.</figcaption>
  </figure>
  <figure>
    <img src="assets/channel-ui-telegram-model.svg" alt="Telegram model selection with inline buttons">
    <figcaption>Model routes use the same command backend as <code>!model use &lt;id&gt;</code>.</figcaption>
  </figure>
  <figure>
    <img src="assets/channel-ui-matrix-fallback.svg" alt="Matrix text fallback for agent and model choices">
    <figcaption>Matrix currently uses deterministic text fallback for bridge-safe operation.</figcaption>
  </figure>
  <figure>
    <img src="assets/channel-ui-signal-fallback.svg" alt="Signal text fallback for agent and model choices">
    <figcaption>Signal is high priority, but remains text-first until native controls are proven.</figcaption>
  </figure>
  <figure>
    <img src="assets/channel-ui-whatsapp-target.svg" alt="WhatsApp native selection target requiring a compatible backend">
    <figcaption>WhatsApp native lists/buttons are a target, not current embedded-channel behavior.</figcaption>
  </figure>
</div>

## Capability Model

| Capability | Native examples | Text fallback |
|---|---|---|
| Choice | Telegram inline keyboard, WhatsApp list/reply buttons, RCS suggested replies | Labeled options with `!agent switch <id>`, `!model use <id>`, or `!switch <agent> <session>` |
| URL/form | Telegram URL button, RCS open URL action | Plain HTTPS link |
| Confirm | Yes/no quick replies or buttons | `!approve <id>` / `!deny <id>` |
| Artifact | Native image, audio, video, or file delivery | Artifact name, size, and safe next action |

## Channel Notes

- Telegram: implemented for agent/model selection, session choices, approval
  decisions, and paste-form links.
- Matrix: keep text-first until native polls/buttons are proven across Element,
  Beeper, and bridges. The Matrix adapter renders the shared choice model as
  deterministic text fallback today.
- Signal: high priority for richer controls where the transport exposes safe
  affordances. The embedded Signal transport currently exposes text sends, so
  Calciforge renders the shared choice model as text fallback.
- WhatsApp: the embedded WhatsApp Web channel is text/media-first today. Native
  list or reply-button support should be tied to a backend that exposes
  WhatsApp interactive messages. Until then, Calciforge renders the shared
  choice model as text fallback.
- SMS: text-only. RCS can support suggested replies/actions through RCS Business
  Messaging providers, but that is a separate rich capability from plain SMS.
- iMessage: useful in theory, but long-term support depends on a provider or
  Apple Messages for Business-style path rather than assuming BlueBubbles will
  stay stable.
