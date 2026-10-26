"""Per-device exit-node state machine.

Shared invariant: at any moment at most one side of the pair may be
USING_OTHER. Both devices USING_OTHER at once would create a routing loop
where outbound traffic has nowhere to go, so transitions into USING_OTHER
are guarded (see coordinator.py).
"""

from __future__ import annotations

from enum import Enum


class ExitState(str, Enum):
    NONE = "none"                # no exit-node relationship with the peer
    USING_OTHER = "using_other"  # this device routes traffic via the peer
    BEING_USED = "being_used"    # the peer routes traffic via this device


# Human-readable labels for the tray menu.
LABELS = {
    ExitState.NONE: "Independent (no exit node)",
    ExitState.USING_OTHER: "Using peer as exit node",
    ExitState.BEING_USED: "Serving as peer's exit node",
}
