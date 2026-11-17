import QtQuick
import qs.Commons
import qs.Commons as Commons
import "../Status.js" as Status

// One agent, as the dashboard draws a row: its state mark, the title, and under
// it the model in its provider's hue and what the agent is doing; the share of
// context used sits at the right.
Item {
  id: root
  property var agent: ({})
  property var strings: null
  property int frame: 0
  property bool animate: true
  property color foreground: Commons.Color.popups.text
  property color hot: Commons.Color.urgent
  property color hue: Commons.Color.muted
  property string fontFamily: Style.font.family

  readonly property bool waiting: agent.state === "waiting"
  readonly property bool queued: agent.state === "queued" || agent.state === "pending"
  readonly property bool working: Status.working(agent)
  // Running, but nothing reported lately: the dashboard's still ring.
  readonly property bool quiet: !waiting && !queued && !working
  // The dim ink Omarchy's panels use for secondary text (PanelHero, PanelSectionHeader).
  readonly property color dim: Qt.darker(foreground, 1.4)
  readonly property bool full: agent.ctx !== null && agent.ctx !== undefined && agent.ctx >= 0.85

  function t(key) { return root.strings ? root.strings.t(key) : "" }

  readonly property string doing: root.waiting ? t("row.asking")
    : root.queued ? t("row.queued")
    : root.quiet ? t("row.quiet")
    : t("now." + root.agent.now)

  implicitHeight: Math.round(Style.font.body * 1.55) + Math.round(Style.font.caption * 1.4)

  Item {
    id: markCell
    width: Style.space(16)
    height: Math.round(Style.font.body * 1.55)

    Rectangle {
      visible: root.waiting
      width: 7; height: 7; rotation: 45; antialiasing: true
      x: Math.round((parent.width - width) / 2); y: Math.round((parent.height - height) / 2)
      color: root.hot
    }
    DotCell {
      visible: root.working
      x: Math.round((parent.width - width) / 2); y: Math.round((parent.height - height) / 2)
      lit: Status.spinDots(root.frame, root.animate)
      color: root.foreground
    }
    Rectangle {
      visible: root.quiet
      width: 7; height: 7; radius: 3.5
      x: Math.round((parent.width - width) / 2); y: Math.round((parent.height - height) / 2)
      color: "transparent"; border.width: 1; border.color: root.dim
    }
    Text {
      visible: root.queued
      anchors.centerIn: parent
      textFormat: Text.PlainText
      text: "·"; color: root.dim
      font.family: root.fontFamily; font.pixelSize: Style.font.body
    }
  }

  Text {
    id: ctx
    anchors.right: parent.right
    y: title.y
    width: Style.space(34)
    height: title.height
    horizontalAlignment: Text.AlignRight
    verticalAlignment: Text.AlignVCenter
    textFormat: Text.PlainText
    text: root.agent.ctx === null || root.agent.ctx === undefined ? "" : Commons.I18n.formatNumber(Math.round(root.agent.ctx * 100)) + "%"
    // A nearly full context is worth noticing, as the dashboard marks it.
    color: root.full ? root.foreground : root.dim
    font.bold: root.full
    font.family: root.fontFamily
    font.pixelSize: Style.font.caption
  }

  Text {
    id: title
    x: markCell.width + Style.space(6)
    width: ctx.x - x - Style.space(8)
    height: markCell.height
    verticalAlignment: Text.AlignVCenter
    textFormat: Text.PlainText
    text: root.agent.title || ""
    elide: Text.ElideRight
    color: root.waiting ? root.hot : root.foreground
    font.family: root.fontFamily
    font.pixelSize: Style.font.body
    font.bold: root.waiting
  }

  Row {
    x: title.x
    y: title.y + title.height
    width: title.width
    spacing: Style.space(6)
    clip: true
    Text {
      visible: text !== ""
      textFormat: Text.PlainText
      text: root.agent.model || ""
      color: root.hue
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
    }
    Text {
      textFormat: Text.PlainText
      text: root.doing
      color: root.waiting ? root.hot : root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
    }
  }
}
