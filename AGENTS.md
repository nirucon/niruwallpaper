# Contributor instructions

- This repository is private during design and implementation. Do not change
  its visibility without the owner's approval.
- Read IMPLEMENTATION-PROMPT.md before implementation. Its future executor must
  receive an explicit instruction to start; committing this brief does not
  authorize changing the installed desktop or other repositories now.
- Inspect repository status and applicable instructions in every target before
  editing. Preserve other contributors' work.
- Keep configuration, credentials, host identities and private planning material
  out of source, fixtures, commits, screenshots and release notes.
- Do not spawn subagents unless explicitly authorized.
- Never grant passwordless sudo execution of user-writable programs. Never run
  a script from a user's installation directory as root to synchronize images.
- Do not move published tags or replace existing release assets. Use a new
  version for corrections.
- Report actual evidence and limitations. A parsed config, listed shortcut,
  running process or successful build is not proof of a working user flow.
