import QtQuick
import Quickshell
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import "Status.js" as Status
import "ui"

// T3 agents at a glance: the dashboard's dot cell, spinning while agents run
// with their count before it, and the bar's alert color with a diamond when
// one needs you. When the monitor can't see T3, an alert icon at full strength
// says so. Click opens a card listing the agents (ui/Card.qml), right-click
// opens the dashboard, middle-click restarts the watcher.
BarWidget {
  id: root
  moduleName: "0xbreadguy.t3-agents"

  readonly property var service: bar && bar.shell ? bar.shell.serviceFor("0xbreadguy.t3-agents") : null
  // Without the service there is nothing to show; say so rather than look idle.
  readonly property var status: service ? service.status : Status.unavailable("", "service")
  readonly property bool broken: !!status && !!status.error
  readonly property bool available: !!status && !status.error
  readonly property int running: available ? status.running : 0
  readonly property int waiting: available ? status.waiting : 0
  readonly property bool quiet: running === 0 && waiting === 0
  // Constant motion stops when animations are turned off (Style.duration
  // returns 0 then); the mark shows one fixed frame instead.
  readonly property bool animate: Style.duration(100) > 0
  property int frame: 0
  property bool cardOpen: false
  readonly property bool cardReady: cardLoader.status === Loader.Ready
  // For the card, which lives in its own file.
  readonly property Item markButton: button
  readonly property var strings: translations

  function n(v) { return Commons.I18n.formatNumber(v) }
  // KeyboardPanel and the bar call this to close the card.
  function close() { root.cardOpen = false }
  function openDashboardFromCard() {
    root.cardOpen = false
    if (root.service) root.service.openDashboard()
  }

  Commons.Translations { id: translations; directory: Qt.resolvedUrl("i18n") }

  // hideWhenIdle never hides a broken monitor: that must not pass for calm.
  visible: root.broken || !(root.setting("hideWhenIdle", false) && root.quiet)
  onVisibleChanged: if (!visible) root.cardOpen = false
  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  // Why there is no status, in plain words.
  function reason() {
    var s = root.status
    if (s.cause === "service") return translations.t("reason.noService")
    if (s.cause === "launcher") return translations.t("reason.noLauncher").arg("~/.local/bin/t3-agents")
    if (s.cause === "exited") return translations.t("reason.exited").arg(s.code === null ? "?" : String(s.code))
    return s.error // the watcher's own message, which names the path it tried
  }

  // One line, like every stock tooltip in the bar: the card has the rest.
  function tooltip() {
    if (root.broken) return translations.t("tooltip.broken")
    if (!root.available) return translations.t("tooltip.starting")
    if (root.quiet) return translations.t("tooltip.idle")
    var parts = []
    if (root.waiting > 0) parts.push(translations.tn("summary.waiting", root.waiting).arg(root.n(root.waiting)))
    if (root.running > 0) parts.push(translations.tn("summary.running", root.running).arg(root.n(root.running)))
    return translations.t("tooltip.active").arg(parts.join(" · "))
  }

  // The bar copies a tooltip's text once, when the pointer arrives
  // (WidgetButton.qml onEntered, Bar.qml showTooltip), so an open tooltip would
  // go stale. Hand it the new text while it is showing.
  readonly property string tip: root.tooltip()
  onTipChanged: if (button.tooltipHovered && root.bar) root.bar.showTooltip(button, root.tip)

  // The frame comes from the clock on a beat shared with the dashboard, so the
  // bar and the dashboard spin in phase. It ticks only while the mark spins or
  // an open card has a spinner in it; idle, waiting or closed, nothing runs.
  Timer {
    interval: 100
    repeat: true
    triggeredOnStart: true
    running: root.visible && root.animate
      && ((root.waiting === 0 && root.running > 0) || (root.cardOpen && Status.cardSpins(root.status)))
    onTriggered: root.frame = Status.frameAt(Date.now())
  }

  BarIconButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    // The cell and the diamond are drawn by the Mark below rather than handed
    // to the button: Omarchy fits an icon component to its measured ink, so a
    // spinner would grow and shrink between frames. The broken state's alert
    // icon is a plain Nerd Font glyph, so the button fits it like a stock icon.
    text: root.broken ? "\u{f0026}" : ""
    iconComponent: root.broken ? null : emptyIcon
    slotSize: root.vertical ? Style.bar.iconSlot : mark.slotWidth
    active: root.waiting > 0
    tooltipText: root.tip
    onPressed: function(code) {
      if (code === Qt.MiddleButton) {
        if (root.service) root.service.restart()
      } else if (code === Qt.LeftButton && root.cardReady) {
        // The card opens without the service too, to say it isn't running.
        root.cardOpen = !root.cardOpen
      } else if (root.service) {
        // Right-click, or a left click when the card couldn't be loaded.
        root.service.openDashboard()
      }
    }

    Mark {
      id: mark
      visible: !root.broken
      button: button
      kind: root.waiting > 0 ? "need" : "cell"
      lit: Status.litDots(root.status, root.frame, root.animate)
      // A vertical bar has no room for a count beside the mark.
      count: root.vertical || root.broken ? "" : (root.waiting > 0 ? root.n(root.waiting) : (root.running > 0 ? root.n(root.running) : ""))
      // Idle and starting recede; running, needs-you and broken never do.
      dim: !root.broken && (!root.available || root.quiet)
    }
  }

  Component { id: emptyIcon; Item {} }

  // The card, from its own file so a missing Omarchy panel part can't take the
  // bar mark down with it (see ui/Card.qml). It is loaded with the widget, as
  // stock panels are, so a failure shows in the shell's log at once rather than
  // on the first click; its contents are built only when it first opens.
  Loader {
    id: cardLoader
    Component.onCompleted: setSource(Qt.resolvedUrl("ui/Card.qml"), { widget: root })
  }
}
