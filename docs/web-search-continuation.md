# Hosted search failures do not terminate coding runs

New runs use tool catalog 16 with limits 14. A committed `WebSearchUncertain` result remains an uncertain tool fact, with its diagnostic, audit and delivery event, but does not transition the coding run to uncertain. The supervisor passes the failure to the next model turn. Citation validation still rejects unusable search output; no trust-warning text is added.

Catalogs through 15 retain their terminal uncertainty invariant. Other uncertain tool results still stop runs. Restart recovery still terminates interrupted runs and never replays an uncertain search; persistence failures and cancellation still stop execution. Continuing inference is not authorization to retry the failed external effect. No credential, route, or isolation boundary changes.
