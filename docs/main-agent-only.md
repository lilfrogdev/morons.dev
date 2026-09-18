# Main-agent-only execution

Morons is removing task delegation. New provider turns expose only `read`, `write`, `edit`, `bash`, `web_search`, and `ipython`; unsolicited `task` calls must fail validation before dispatch. The main agent inspects, implements, and verifies work directly.

This narrows execution capabilities, not local tool authority: tools still run with the owner's normal authority, without sandboxing or rollback. Historical task inputs, child records, model bindings, and results remain validated and readable. Startup must still terminate interrupted historical children before tools and parent runs, without replay. Removing the feature must not discard that recovery or weaken database validation.

Subagent settings and execution controls are being retired; stored historical settings do not authorize new delegation.
