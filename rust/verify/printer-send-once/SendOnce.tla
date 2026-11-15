---- MODULE SendOnce ----
\* One queued printer send, as Print Partner actually steps it.
\*
\* Each action is one indivisible step in the code:
\*   SendNow            sendable + dispatch          writes.rs sendable, dispatch
\*   FailBeforeEffect   FailWith before BeginEffect  send_worker.rs give_up
\*   BeginEffect        WorkerOperation::BeginEffect send_worker.rs begin
\*   Upload             HostSends::upload            send_worker.rs run, one call
\*   Confirm            ConfirmEffect + FinishPublic send_worker.rs Sent::Done
\*   Deny               DenyEffect                   send_worker.rs Sent::Failed
\*   FailUnknown        Fail, or recovery_decision   jobs.rs Fail / recovery_decision
\*   ReconcilePrinted   reconcile printed=true       writes.rs reconcile
\*   ReconcileNotPrinted reconcile printed=false     writes.rs reconcile
\*   Finished           the item is done             no further send
\*
\* `faithful` is the sendable() guard: Send now is refused while the last
\* outcome is unknown (OUTCOME_UNKNOWN / send_outcome_unknown). FALSE is the
\* broken variant with that guard removed. It is not the product.

EXTENDS Integers

CONSTANT faithful

VARIABLES item, handed, uploads, unknown, frozen

vars == <<item, handed, uploads, unknown, frozen>>

Init ==
    /\ item = "queued"
    /\ handed = FALSE
    /\ uploads = 0
    /\ unknown = FALSE
    /\ frozen = 0

SendNow ==
    /\ item \in {"queued", "error"}
    /\ (faithful => ~unknown)
    /\ item' = "running"
    /\ UNCHANGED <<handed, uploads, unknown, frozen>>

FailBeforeEffect ==
    /\ item = "running"
    /\ ~handed
    /\ item' = "error"
    /\ unknown' = FALSE
    /\ UNCHANGED <<handed, uploads, frozen>>

BeginEffect ==
    /\ item = "running"
    /\ ~handed
    /\ item' = "effect_admitted"
    /\ UNCHANGED <<handed, uploads, unknown, frozen>>

Upload ==
    /\ item = "effect_admitted"
    /\ ~handed
    /\ handed' = TRUE
    /\ uploads' = uploads + 1
    /\ UNCHANGED <<item, unknown, frozen>>

Confirm ==
    /\ item = "effect_admitted"
    /\ handed
    /\ item' = "done"
    /\ handed' = FALSE
    /\ unknown' = FALSE
    /\ UNCHANGED <<uploads, frozen>>

Deny ==
    /\ item = "effect_admitted"
    /\ handed
    /\ item' = "error"
    /\ handed' = FALSE
    /\ unknown' = FALSE
    /\ UNCHANGED <<uploads, frozen>>

\* Effect already admitted. Unknown outcome, or a crash recovered by
\* recovery_decision: ReconciliationRequired, and the file is not sent again.
FailUnknown ==
    /\ item = "effect_admitted"
    /\ item' = "error"
    /\ handed' = FALSE
    /\ unknown' = TRUE
    /\ frozen' = uploads
    /\ UNCHANGED <<uploads>>

ReconcilePrinted ==
    /\ unknown
    /\ item = "error"
    /\ item' = "done"
    /\ unknown' = FALSE
    /\ UNCHANGED <<handed, uploads, frozen>>

ReconcileNotPrinted ==
    /\ unknown
    /\ item = "error"
    /\ unknown' = FALSE
    /\ UNCHANGED <<item, handed, uploads, frozen>>

Finished ==
    /\ item = "done"
    /\ UNCHANGED vars

Next ==
    \/ SendNow
    \/ FailBeforeEffect
    \/ BeginEffect
    \/ Upload
    \/ Confirm
    \/ Deny
    \/ FailUnknown
    \/ ReconcilePrinted
    \/ ReconcileNotPrinted
    \/ Finished

Spec == Init /\ [][Next]_vars

\* A user may send again after they reconcile "not printed". Two sends cover
\* that path. Further sends are the same cycle, not a new case.
Bound == uploads <= 2

\* While the last outcome is unknown, HostSends::upload has not been called again.
NoResendWhileUnknown == unknown => uploads = frozen

\* Send now does not start another attempt while that outcome is still unknown.
SendNowRefusesUnknown == ~(unknown /\ item = "running")

====
