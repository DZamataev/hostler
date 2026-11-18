# Smoke test: the macOS app

The AppKit glue that has no unit test (the status item, the popover, the Dock icon, window
raising) and the whole app around it are checked here by hand on a real Mac, as the reference app's
`docs/SMOKE_TEST.md` does. Run it after any change under `macos/Sources/Hostler/App/`, after a
change to a view, and before a release.

Every check has **Steps** and **Expected**. A check fails when any expected line does not hold;
record the failure with the macOS version and the build you ran.

## Setup

1. Build the app and the CLI from the same checkout:

   ```sh
   cargo build
   scripts/macos.sh app      # Release: tmp/app/Hostler.app
   scripts/macos.sh build    # Debug with the test hooks: tmp/target-xcode/Build/Products/Debug/
   ```

2. A **scratch service**, never the owner's registered one: a short private home under `/tmp`
   with mode 700, started from a terminal:

   ```sh
   SCRATCH=$(mktemp -d /tmp/hostler-smoke.XXXXXX)
   chmod 700 "$SCRATCH"
   HOSTLER_HOME="$SCRATCH" target/debug/hostler service start
   ```

   The app is then opened with the same home (`open --env` passes it to the app; Task 0 confirmed
   it):

   ```sh
   open --env HOSTLER_HOME="$SCRATCH" tmp/app/Hostler.app
   ```

3. A menu-bar manager (Ice, Bartender) may hide a new status item: move hostler's into the
   visible section first. AX presses work either way.

4. When done: quit the app (popover › Quit, or `kill <pid>` for the app you launched), stop the
   service by its home (`HOSTLER_HOME="$SCRATCH" target/debug/hostler service stop`), and
   `rm -rf "$SCRATCH"`. Never stop or restart a service you did not start.

## 1. The status item and the popover

Each step starts from the popover closed unless it says otherwise.

- [ ] **Launch shows nothing else.** Steps: quit the app, then open it. Expected: no window
      opens; the Dock has no hostler icon; the status item appears in the menu bar.
- [ ] **A click opens the popover under the item.** Steps: click the status item once. Expected:
      the panel opens directly under the item, arrow included, and is key (⌘Q in it quits).
- [ ] **A second click closes it.** Steps: click the item again. Expected: the panel closes and
      stays closed (the mouse-up of the closing click does not reopen it).
- [ ] **A right click does the same as a left click.** Steps: right-click the item while the
      panel is closed, then while it is open. Expected: it opens, then closes.
- [ ] **A fast double click does not flicker.** Steps: double-click the item quickly. Expected:
      the panel ends in one clear state (closed after it opened), never oscillating.
- [ ] **Escape closes it.** Steps: open the panel, press Escape. Expected: it closes.
- [ ] **A click in another app closes it.** Steps: open the panel, click a visible window of
      another app. Expected: the panel closes before/while that click is delivered.
- [ ] **⌘Tab closes it.** Steps: open the panel, press ⌘Tab. Expected: it closes.
- [ ] **A click in our own window closes it.** Steps: open the main window, then the panel, then
      click in the window. Expected: the panel closes; the window stays in front.
- [ ] **Another display's menu bar closes it.** Steps: with two displays, open the panel on
      display A, click the menu bar of display B. Expected: the panel closes instead of moving.
- [ ] **A short display scrolls.** Steps: with the popover full of requests (or on a short
      display), open it. Expected: the lower region scrolls; the header, the banner and the first
      cards stay; the panel never runs under the Dock or off the screen.
- [ ] **The highlight never sticks.** Steps: close the panel every way above. Expected: after
      each, the status item is drawn unpressed.
- [ ] **The image follows the menu bar.** Steps: switch the system between light and dark
      appearance, then stop the service. Expected: the glyph is a template (its contrast follows
      the menu bar), its states (idle, attention with a dot, offline dimmed) change the image,
      and the item's width never changes.

## 2. The connection, Start, and where it is offered

- [ ] **Nothing starts by itself.** Steps: with the scratch service stopped, launch the app.
      Expected: no service process appears (`pgrep -fl 'hostler service'`), and the popover shows
      only the banner for a custom home: the title that the service is not running, the command
      to type (`HOSTLER_HOME=… hostler service run`) with **Copy** and **Retry**, and **no Start
      button** (spec §9.7: Start is offered only on the default home).
- [ ] **Copy and Retry.** Steps: press Copy; paste it into a terminal. Expected: exactly the
      command shown, with the scratch home. Press Retry after starting the service from that
      terminal. Expected: Retry connects within a few seconds without relaunching the app.
- [ ] **Start only on the default home.** Steps: quit the app, then open it **without**
      `HOSTLER_HOME` (its normal home, whose service is installed and stopped), open the popover.
      Expected: the banner offers **Start**; pressing it starts the registered service and the
      popover becomes live. Stop it again from the terminal (`hostler service stop`). The app
      never starts a service on its own at launch or when the popover opens.
- [ ] **A refused connection offers no Start.** Steps: with the socket pointing at a directory
      the user may not read (or a service of another user, as in the unit tests), open the
      popover. Expected: the refusal banner with the reason, Retry only.
- [ ] **A contract mismatch is not repaired.** Steps: run a service with
      `HOSTLER_TEST_CONTRACT_VERSION=4` on the scratch home. Expected: the mismatch banner names
      both contract versions and the service version, offers Copy of the recovery and Retry, and
      never replaces or restarts the service.
- [ ] **The header state is honest.** Expected while live: a green dot and **Live**; while
      stopped: gray and **Offline**; while refused: red and **Refused**.

## 3. The popover's values (Task 9)

Against the scratch service with a throwaway database:

- [ ] **Unlock.** Steps: with the database locked, press **Unlock…**. Expected: the form opens
      inline in the popover; the master-password field takes typing; a wrong password shows the
      service's message and clears the field; the right password unlocks, closes the form, and
      the lock row says **Secrets unlocked**.
- [ ] **Create.** Steps: with no database, press **Create…**. Expected: the form asks for the
      password twice; different passwords show an error and send nothing; the same password
      twice creates the database and unlocks it.
- [ ] **A key file.** Steps: press **Choose…**. Expected: the popover closes, the open panel
      appears, choosing a file brings the popover back with the form still open and the file's
      name shown; **Clear** removes it.
- [ ] **Lock asks first.** Steps: press **Lock**. Expected: an inline confirmation with
      "Agents' runs that need secrets will be refused." and Cancel/Lock; Cancel keeps it
      unlocked; Lock locks it and the row says **Secrets locked**.
- [ ] **A secret request.** Steps: in an approved project,
      `hostler secret request API_KEY --reason "smoke test"`. Expected: a card in the popover
      with the name, the declared purpose, the reason in quotes, project, pid and age; typing a
      value and **Store** answers it (the CLI prints `stored`); **Decline** answers `declined`.
      Closing the popover with something typed in the field clears it.
- [ ] **An account request.** Steps: `hostler account request qa-user --field session --reason
      "smoke"`. Expected: a card with one secure field per requested field; **Store** runs the
      person gate (section 4) and stores all fields in one save; the CLI prints the answered
      request and `stored` fields.
- [ ] **Nothing typed survives closing.** Steps: type into the unlock form, a request card and
      the gate, then close the popover. Expected: every field is empty when it reopens.

## 4. The person gate and the lockout text (spec §8.5)

- [ ] **A gated action asks when no grant is live.** Steps: lock the database (or wait for the
      grant to expire), then answer an account request or reveal a value. Expected: the sheet/inline
      form says what the action is for, the explanation ("This confirms it is you. It does not stop
      other programs of your user from using hostler's socket."), the master-password field,
      Cancel and Confirm; Confirm stays disabled until something is typed.
- [ ] **A wrong password keeps the form open.** Steps: type a wrong password, Confirm. Expected:
      the service's message, the field cleared, the form still open; nothing was sent.
- [ ] **Five quick wrong passwords show the limit.** Steps: five quick wrong passwords. Expected:
      "Too many wrong passwords; next try in N s. Another process of your user may be guessing."
      counting down once a second, with Confirm disabled until it ends.
- [ ] **A live grant skips the form.** Steps: unlock, then reveal within the grant's life.
      Expected: the value appears without asking again; after the grant expires, the form asks
      once more.
- [ ] **Two surfaces cannot both ask.** Steps: start a gated action in the window and immediately
      one in the popover (or vice versa). Expected: the second says another action already waits
      for the master password; only one sheet is up.
- [ ] **Closing the surface cancels.** Steps: open the gate in the popover, close the popover.
      Expected: nothing is sent, and the next gated action asks normally.
- [ ] **The lockout text counts down and re-enables.** Expected: Confirm becomes available when
      the countdown ends, without reopening the app.

## 5. Reveal and the clipboard (spec §9.6)

- [ ] **Reveal shows the value for 30 s.** Steps: window › Secrets (or Accounts) › **Reveal**,
      pass the gate. Expected: the value appears monospaced for one field (all fields of an
      account), with "Hidden again after 30 s" and **Hide**; it disappears after about 30 s.
- [ ] **Only one reveal at a time.** Steps: reveal one secret, then another. Expected: the first
      hides; only the newer value is on screen.
- [ ] **A lock hides it at once.** Steps: reveal, then lock the database from the popover or
      another client. Expected: the value disappears immediately.
- [ ] **Closing the window hides it.** Steps: reveal, close the window. Expected: the value is
      gone when the window reopens (no value is redrawn).
- [ ] **It is not in the accessibility tree.** Steps: reveal, read the app's AX tree with
      Accessibility Inspector (or cua-driver's `get_window_state`). Expected: the Copy and Hide
      buttons and the 30 s line are there; the value's text is not.
- [ ] **It is not in a screenshot.** Steps: reveal, take a screenshot of the screen (⌘⇧3) or
      capture the window through cua-driver. Expected: the window is left out of the capture
      (`sharingType = .none` while the value is shown; best effort); the window is captured
      normally again after Hide.
- [ ] **Copy is concealed and clears after 60 s.** Steps: press **Copy** on a revealed field;
      paste it somewhere immediately. Expected: the value pastes correctly; the pasteboard lists
      `org.nspasteboard.ConcealedType` (pasteboard viewers show it concealed); 60 s later the
      pasteboard is empty **unless** something else was copied since (then that copy stays).
- [ ] **Quit clears what hostler copied.** Steps: Copy, then quit the app within a minute.
      Expected: the pasteboard is cleared if it still holds hostler's copy.

## 6. Settings and notifications (spec §9.4)

- [ ] **The four kinds persist.** Steps: Settings… (⌘,), toggle each of the four kinds, quit and
      reopen the app. Expected: each toggle keeps its state.
- [ ] **The permission line is honest.** Expected: `unknown`/`notAsked` gives "macOS asks for
      permission when a kind is turned on."; while the macOS prompt is up, "Waiting for your
      answer in the macOS prompt."; denied gives the System Settings path; a refused build gives
      "macOS refused notifications for this build (…)". Turning a kind on asks macOS once.
- [ ] **A new device and a request post a notification.** Steps: with the request kind on and a
      secret request pending, run `hostler secret request …` in a second project. Expected: one
      banner with a generic title and body (no reason, name, path or command) and a click opens
      the popover at the request.
- [ ] **A stuck task posts one notification.** Steps: run a heavy task and let it pass the stuck
      threshold. Expected: one banner, a click selects Queue in the window; the same task never
      notifies twice.
- [ ] **The login item is off outside /Applications.** Steps: run the app from `tmp/app` and open
      Settings. Expected: **Open at login** is disabled with "Install the app in /Applications
      first (scripts/macos.sh install)."
- [ ] **The login item registers from /Applications.** Steps: `scripts/macos.sh install`, open
      `/Applications/Hostler.app`, turn **Open at login** on. Expected: System Settings ›
      General › Login Items lists hostler; turning it off removes it; the toggle shows macOS's
      truth (including "waiting for approval" when macOS asks for the person's approval).
      Uninstall the copy when done if the machine should not keep it.

## 7. The window and the Dock icon (spec §9.3)

- [ ] **Open hostler.** Steps: popover › Open hostler (or ⌘O). Expected: the popover closes, the
      window opens at All Devices, the Dock icon appears.
- [ ] **The window's content.** Expected: the sidebar's top group (All Devices, Queue, Screen,
      Accounts, Secrets, Projects) and one section per project with its workspaces; the footer
      says Live (or Offline since HH:MM) and Refresh re-reads every snapshot (⌘R).
- [ ] **All Devices.** Expected: the table's Device, Type, State, Standing and Battery columns
      sort; the segmented filters work; unregistered devices appear only with the eye toggle
      (**Unregistered** in its tooltip) on; a row opens the device detail with its telemetry,
      claims, sessions, installed apps, note and history.
- [ ] **The toolbar at the minimum width.** Steps: drag the window's corner to its minimum
      (1100 × 620), in English and then in Russian. Expected: both segmented filters, the eye
      toggle and Refresh are fully visible in one row, never overlapping; the Russian segments
      ("Физические", "Виртуальные") fit inside their picker; nothing folds into the toolbar's
      overflow menu. The window refuses to shrink below the minimum.
- [ ] **Workspace detail.** Expected: Finder and Terminal open the checkout; held, waiting, heavy
      tasks, other devices, secrets in short and health sections; Reserve/Unreserve, Renew,
      Release…, Withdraw, Take over… work and each confirmation names its CLI command.
- [ ] **Queue.** Expected: the All/Running/Waiting/Finished segments count their rows; a waiting
      task can be withdrawn; a running hostler task's Stop… asks for its id, passes the gate,
      then records it stopped; a foreign (rn-looky) task offers neither.
- [ ] **Screen and Accounts.** Expected: the Screen page shows the holder, its TTL and who waits;
      Take over… asks for the workspace and the typed token. Accounts lists fields set or
      missing, reveals through the gate, and sets/removes a field.
- [ ] **Secrets and Projects.** Expected: Secrets lists declared and stored names with set or
      missing, Set… writes a value, Reveal is gated, Share with…/Unshare… and Delete… work, and
      the preserved-files list deletes only behind the gate; Projects shows approval, workspaces,
      and `hostler doctor`'s checks with a Copy for each fix command (the app never runs --fix).
- [ ] **Closing hides the Dock icon.** Steps: close the window. Expected: the Dock icon disappears;
      the app keeps running and the status item stays.
- [ ] **Settings holds the icon while the window is open.** Steps: open the window, then Settings
      (⌘,), close one, then the other. Expected: the icon stays until both are closed.
- [ ] **Quit.** Steps: popover › Quit hostler (⌘Q). Expected: the app quits; the window and
      status item go; no process is left.

## 8. Opening the running app again

- [ ] **Window behind another app.** Steps: open the window, switch to another app, then open
      hostler again (Finder, Spotlight, `open`). Expected: the window comes forward; the popover
      does not open.
- [ ] **No window.** Steps: close the window, then open hostler again. Expected: the popover opens
      (the way back when the status item is hidden).
- [ ] **Already front with its window.** Steps: with the app in front and its window open, open
      the app again. Expected: the popover opens.

## 9. Debug-only hooks (lead rulings P5-L5, P5-L6)

Only a Debug build reads these; a Release build ignores them. Never install a Debug build in
/Applications.

- [ ] **The popover hook.** Steps: `open --env HOSTLER_APP_TEST_OPEN_POPOVER=1
      --env HOSTLER_HOME="$SCRATCH" <Debug>/Hostler.app`. Expected: the popover opens about one
      second after launch, with no window.
- [ ] **The keep hook.** Steps: add `--env HOSTLER_APP_TEST_KEEP_POPOVER=1` and switch to another
      app. Expected: the popover stays open; a click elsewhere and Escape still close it.
- [ ] **The Debug menu.** Steps: with the main window open in a Debug build, open the menu bar.
      Expected: **Debug › Reveal Popover** opens the popover (also reachable through the window's
      accessibility menu bar); reopening the app still raises the window.
- [ ] **Release ignores the hooks.** Steps: run `scripts/macos.sh app`, open the Release app with
      both variables. Expected: it behaves exactly as without them; its menu bar has no Debug
      menu; `scripts/macos.sh app` itself fails if the Release binary contains either variable
      name or the Debug menu's marker.

## 10. Russian (spec §9.5)

The interface follows the system language; CLI output and the contract stay English.

- [ ] **The popover in Russian.** Steps: `open --env HOSTLER_HOME="$SCRATCH"
      tmp/app/Hostler.app --args -AppleLanguages '(ru)'` (or set the app's language in System
      Settings › General › Language & Region › Applications). Expected: the
      header, banner, commands, unlock form and cards are Russian; `hostler …` commands, method
      names, file paths, service messages and device names are unchanged.
- [ ] **The window and sheets in Russian.** Expected: the sidebar, every section, the
      confirmation sheets, alerts, Settings and notifications are Russian; at the minimum width
      the toolbar's two filters, the eye toggle and Refresh still fit without overlapping.
- [ ] **No English left.** Expected: no sentence of the app's own is English in a Russian run
      (technical tokens such as hostler, iOS, Android, Wi-Fi, agent-device and commands are
      expected to stay as they are).

## 11. Quit and cleanup

- [ ] **Nothing is left behind.** Steps: quit the app, stop the scratch service, remove the
      scratch home. Expected: no `Hostler` app process, no service for the scratch home, and the
      owner's registered service is untouched (`launchctl print gui/$(id -u)/io.github.dzamataev.hostler`
      shows its pid unchanged).
- [ ] **No value was printed anywhere.** Expected: nothing in the run printed, logged or copied
      a real value; the throwaway values used above are safe to discard.
