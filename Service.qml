import QtQuick
import Quickshell
import Quickshell.Io
import "Status.js" as Status

// One watcher for every bar on every screen. `loupe --status --watch` reads
// T3's database only after T3 has written to it, and prints a line only when the
// status changes, so an idle machine costs next to nothing.
Item {
  id: root

  property var shell: null
  property var manifest: null

  // Last status from the watcher (see Status.parse), an unavailable status when
  // the watcher cannot run, or null until its first line.
  property var status: null
  readonly property string launcher: Quickshell.env("HOME") + "/.local/bin/loupe"
  property bool started: false // the current watcher run got as far as starting
  property bool restarting: false // a stop asked for by restart(), not a failure

  function openDashboard() {
    // Fixed argv, no shell: the launcher focuses an open dashboard or opens one.
    Quickshell.execDetached([root.launcher, "--window"])
  }

  function restart() {
    if (!watcher.running) {
      watcher.running = true
      return
    }
    root.restarting = true
    watcher.running = false // the stop handler starts it again
  }

  Process {
    id: watcher
    command: [root.launcher, "--status", "--watch"]
    running: true
    stdout: SplitParser {
      onRead: function(data) {
        var next = Status.parse(data)
        if (next) root.status = next
      }
    }
    onStarted: root.started = true
    onExited: function(exitCode) {
      if (root.restarting) return
      // Keep the watcher's own last words (say, missing binary or unreadable DB) over a bare exit code.
      if (!root.status || !root.status.error) root.status = Status.unavailable("exit code " + exitCode, "exited", exitCode)
    }
    onRunningChanged: {
      if (running) return
      if (root.restarting) {
        root.restarting = false
        root.started = false
        Qt.callLater(function() { watcher.running = true })
        return
      }
      // Quickshell reports a launcher that cannot start (not installed yet) only
      // by stopping, with no exit code and no output.
      if (!root.started) root.status = Status.unavailable("cannot start " + root.launcher, "launcher")
      root.started = false
    }
  }

  // Keep trying while the watcher is down, so installing the dashboard after the
  // widget, or a crash, recovers without a shell restart.
  Timer {
    interval: 15000
    repeat: true
    running: true
    onTriggered: {
      if (watcher.running) return
      // Stopped without a recorded reason: it never managed to start.
      if (!root.status || !root.status.error) root.status = Status.unavailable("cannot start " + root.launcher, "launcher")
      watcher.running = true
    }
  }
}
