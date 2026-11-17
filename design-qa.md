# CNB Account Menu Condensed Design QA

## Evidence

- Before screenshot: `C:\Users\laojiu\AppData\Local\Temp\codex-ui-paste\pasted-7956-1794896773140-3.png`
- Implementation screenshot: `target/design-qa/implementation-account-menu-condensed.png`
- Combined comparison: `target/design-qa/comparison-account-menu-condensed.png`
- Full-width control comparison: `target/design-qa/comparison-account-menu-refined-final.png`
- Viewport: 1319 x 594 CSS pixels, device scale factor 1.
- State: signed out, account popover open, first saved account focused.

Computer Use was unavailable, so the production Vue component was rendered with representative account data in Chrome headless at the app’s target viewport.

## Findings

- No actionable P0, P1, or P2 findings remain.
- The trigger no longer renders the saved-account count subtitle.
- The menu header no longer renders the “选择账号继续登录” helper text.
- Account rows no longer render login time or “旧版账号”.
- User rows are vertically centered with a tighter approximately 52px rhythm.
- The quantity badge, username, remove action, scan action, bordered trigger, user icon, and chevron remain.

## Fidelity Review

- Typography: only primary labels and usernames remain in the account selector; no stale two-line spacing is visible.
- Spacing: the panel is visibly shorter and the title, rows, divider, and scan action still maintain consistent vertical rhythm.
- Colors: the existing dark panel, raised trigger, accent focus, muted remove icon, and danger hover remain unchanged.
- Assets: account and scan icons continue to use the established icon library.
- Copy: “选择 CNB 账号”, “已保存账号”, “扫码添加账号”, and the count badge are the only intended account-layer labels.

## Comparison History

1. The prior implementation included a trigger subtitle, menu helper text, and two-line account rows.
2. Those three small-text groups were removed and row spacing was tightened.
3. The revised capture was compared with the before state; no P0/P1/P2 visual or alignment issues remained.

## Verification

- Component tests assert the helper strings, login times, and “旧版账号” are absent.
- Component tests still cover login, busy state, removal, scan, Escape, and focus return.
- App tests cover selected-account login, removal confirmation, empty state, and settings behavior.

Final result: passed
