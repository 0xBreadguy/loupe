import QtQuick
import Quickshell
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import "Status.js" as Status

// T3 agents at a glance: a spinner and a count while agents run, and the bar's
// alert color with a diamond when one needs you. Click opens the dashboard.
BarWidget {
  id: root
  moduleName: "0xbreadguy.t3-agents"

  readonly property var service: bar && bar.shell ? bar.shell.serviceFor("0xbreadguy.t3-agents") : null
  readonly property var status: service ? service.status : null
  readonly property bool available: !!status && !status.error
  readonly property int running: available ? status.running : 0
  readonly property int waiting: available ? status.waiting : 0
  readonly property bool quiet: running === 0 && waiting === 0
  // The spinner is constant motion, so it stops when animations are turned off.
  readonly property bool animate: Style.duration(100) > 0
  readonly property string count: waiting > 0 ? Commons.I18n.formatNumber(waiting)
    : (running > 0 ? Commons.I18n.formatNumber(running) : "")
  property int frame: 0

  Commons.Translations { id: strings; directory: Qt.resolvedUrl("i18n") }

  visible: !(root.setting("hideWhenIdle", false) && root.quiet)
  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  function tooltip() {
    if (!root.service) return strings.t("tooltip.noService")
    if (!root.available) return strings.t("tooltip.unavailable")
    var format = function(n) { return Commons.I18n.formatNumber(n) }
    var counts = []
    if (root.waiting > 0) counts.push(strings.tn("tooltip.waiting", root.waiting).arg(format(root.waiting)))
    if (root.running > 0) counts.push(strings.tn("tooltip.running", root.running).arg(format(root.running)))
    var lines = [strings.t("tooltip.title"), counts.length ? counts.join(" · ") : strings.t("tooltip.idle")]
    var shown = root.status.agents.slice(0, 6)
    for (var i = 0; i < shown.length; i++) lines.push(Status.agentLine(shown[i], format))
    var extra = root.running + root.waiting - shown.length
    if (extra > 0) lines.push(strings.tn("tooltip.more", extra).arg(format(extra)))
    if (root.status.failed > 0) lines.push(strings.tn("tooltip.failed", root.status.failed).arg(format(root.status.failed)))
    lines.push(strings.t("tooltip.hint"))
    return lines.join("\n")
  }

  Timer {
    interval: 100
    repeat: true
    running: root.visible && root.animate && root.waiting === 0 && root.running > 0
    onTriggered: root.frame = (root.frame + 1) % 10
  }

  BarIconButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    // The mark is drawn below rather than handed to the button: Omarchy fits an
    // icon to its measured ink, and spinner frames differ in ink, so a fitted
    // spinner would grow and shrink on every frame.
    iconComponent: Component { Item {} }
    slotSize: root.vertical || root.count === "" ? Style.bar.iconSlot : Math.ceil(mark.implicitWidth + Style.space(12))
    active: root.waiting > 0
    tooltipText: root.tooltip()
    onPressed: function(code) {
      if (!root.service) return
      if (code === Qt.MiddleButton) root.service.restart()
      else root.service.openDashboard()
    }

    Row {
      id: mark
      x: (button.width - width) / 2
      y: (button.height - height) / 2
      spacing: Style.space(3)
      scale: button.pressScale
      opacity: root.available && !root.quiet ? 1 : 0.45

      Text {
        anchors.verticalCenter: parent.verticalCenter
        textFormat: Text.PlainText
        text: Status.glyph(root.status, root.frame, root.animate)
        color: root.waiting > 0 ? button.activeColor : button.foreground
        font.family: button.fontFamily
        font.pixelSize: button.fontSize
      }

      Text {
        anchors.verticalCenter: parent.verticalCenter
        visible: !root.vertical && root.count !== ""
        textFormat: Text.PlainText
        text: root.count
        color: root.waiting > 0 ? button.activeColor : button.foreground
        font.family: button.fontFamily
        font.pixelSize: Math.round(button.fontSize * 0.86)
      }
    }
  }
}
