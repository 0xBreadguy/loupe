import QtQuick
import Quickshell
import Quickshell.Io
import "Status.js" as Status

// One watcher for every bar on every screen. `t3-agents --status --watch` reads
// T3's database only after T3 has written to it, and prints a line only when the
// status changes, so an idle machine costs next to nothing.
Item {
  id: root

  property var shell: null
  property var manifest: null

  // Last status from the watcher (see Status.parse), or null before the first line.
  property var status: null
  readonly property string launcher: Quickshell.env("HOME") + "/.local/bin/t3-agents"

  function openDashboard() {
    // Fixed argv, no shell: the launcher focuses an open dashboard or opens one.
    Quickshell.execDetached([root.launcher, "--window"])
  }

  function restart() {
    retry.stop()
    watcher.running = false
    Qt.callLater(function() { watcher.running = true })
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
    onExited: function(exitCode) {
      root.status = Status.unavailable("watcher exited with code " + exitCode)
      retry.restart()
    }
  }

  // Back off when the launcher is missing or the database cannot be read.
  Timer {
    id: retry
    interval: 15000
    onTriggered: watcher.running = true
  }
}
