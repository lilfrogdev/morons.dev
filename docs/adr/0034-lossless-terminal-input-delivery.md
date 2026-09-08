# ADR 0034: Bounded lossless terminal input delivery

## Status and evidence

Accepted before implementation. An owner-authorized text-only native smoke attempt submitted a truncated prompt. Source inspection independently establishes that the16-slot terminal event queue uses try_send and discards Full errors for keys, paste, images and mouse input. Such loss can alter a user's command or turn a later Enter into submission of unintended text. The observed native provider-protocol rejection is a separate failure, not explained by the input truncation.

## Decision

Keep the existing16-slot queue and one owned polling thread, but apply backpressure when delivering terminal input rather than silently dropping it. Preserve event order and existing paste/image bounds and redaction. The reader retains at most one pending decoded event while blocked, then stops reading the OS event source until the consumer makes room. Do not enlarge the queue, busy-spin, create an input/run queue, synthesize a key, auto-submit or retry a request.

TerminalEvents Drop first signals stop and closes its receiver, then joins its reader. Receiver closure wakes a blocked send even while queued events remain, avoiding join-before-close deadlock. Shutdown may discard pending input because the terminal consumer is ending; it must not execute that input later. Polling deadlines and clipboard behavior remain unchanged; this is lifecycle control, not a sandbox or a guarantee against OS/terminal input loss outside Morons.

## Qualification

Reproduce Full-queue loss before changing delivery. Test burst key/paste ordering with a deliberately stalled consumer, bounded queue pressure, closed-receiver termination, and Drop with a blocked reader. Do not read a real terminal/clipboard or submit model requests in these tests. After complete local/platform gates, verify a harmless draft's exact displayed bytes without Enter before any newly authorized inference sample. Preserve the already submitted canonical text and failed provider operation; never rewrite or automatically replay it.
