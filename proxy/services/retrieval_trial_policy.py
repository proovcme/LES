"""Explicit offline ablations; no environment switch or production default change."""
from dataclasses import dataclass, replace


@dataclass(frozen=True)
class RetrievalTrialPolicy:
    name: str = "current"
    hierarchy: bool = True
    lexical_safety: bool = True
    exact_promotions: bool = True
    table_appendix: bool = True
    weak_retry: bool = True
    raptor: bool = True
    colbert: bool = True


CURRENT = RetrievalTrialPolicy()
MINIMAL = RetrievalTrialPolicy(
    name="minimal", hierarchy=False, lexical_safety=False, exact_promotions=False,
    table_appendix=False, weak_retry=False, raptor=False, colbert=False,
)
TRIAL_PROFILES = {profile.name: profile for profile in (
    CURRENT, MINIMAL, replace(MINIMAL, name="hierarchy", hierarchy=True),
    replace(CURRENT, name="without_lexical_safety", lexical_safety=False),
    replace(CURRENT, name="without_exact_promotions", exact_promotions=False),
    replace(CURRENT, name="without_retry", weak_retry=False),
    replace(CURRENT, name="without_advanced", raptor=False, colbert=False),
)}
