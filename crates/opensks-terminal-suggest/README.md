# OpenSKS Terminal Suggest

Deterministic terminal suggestion engine for OpenSKS terminal input.

This crate proposes command replacements while the user is typing. It does not
execute commands and it does not call a provider directly. Provider-backed AI
command proposals can be normalized into the same suggestion contract by a
future adapter, and risky suggestions remain approval-gated.
