"""Check the public web-link contract used by URLCheck and Android choosers."""

from pathlib import Path
from xml.etree import ElementTree


ROOT = Path(__file__).resolve().parents[1]
ANDROID = "{http://schemas.android.com/apk/res/android}"


def attribute(element, name):
    return element.attrib.get(ANDROID + name)


def main():
    manifest = ElementTree.parse(ROOT / "app/src/main/AndroidManifest.xml")
    application = manifest.getroot().find("application")
    assert application is not None
    activities = {
        attribute(activity, "name"): activity
        for activity in application.findall("activity")
    }
    assert ".MainActivity" in activities
    receiver = activities[".DownloadLinkActivity"]
    assert attribute(receiver, "exported") == "true"
    assert attribute(receiver, "label") == "@string/download_with_fetchrail"
    assert attribute(receiver, "theme") == "@android:style/Theme.NoDisplay"

    filters = receiver.findall("intent-filter")
    assert len(filters) == 1
    link_filter = filters[0]
    assert attribute(link_filter, "autoVerify") != "true", "Not a browser default"
    assert {attribute(node, "name") for node in link_filter.findall("action")} == {
        "android.intent.action.VIEW"
    }
    assert {attribute(node, "name") for node in link_filter.findall("category")} == {
        "android.intent.category.DEFAULT",
        "android.intent.category.BROWSABLE",
    }
    assert {attribute(node, "scheme") for node in link_filter.findall("data")} == {
        "http",
        "https",
    }
    assert all(attribute(node, "mimeType") is None for node in link_filter.findall("data"))
    assert all(attribute(node, "host") is None for node in link_filter.findall("data"))

    launcher_filters = activities[".MainActivity"].findall("intent-filter")
    main_actions = {
        attribute(action, "name")
        for intent_filter in launcher_filters
        for action in intent_filter.findall("action")
    }
    assert main_actions == {"android.intent.action.MAIN", "android.intent.action.SEND"}
    assert (
        "Download with Fetchrail"
        in (ROOT / "app/src/main/res/values/strings.xml").read_text(encoding="utf-8")
    )
    print("PASS: URLCheck ACTION_VIEW chooser contract and named download target")


if __name__ == "__main__":
    main()
