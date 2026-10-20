/** @odoo-module **/

import { registry } from "@web/core/registry";
import { ThingField, thingField } from "./component";
import legacyFields from "web.field_registry";

export const fieldRegistry = registry.category("fields");
const viewRegistry = registry.category("views");

registry.category("fields").add("thing", thingField);
registry.category("debug").category("form").add("thing_debug", (env) => env.debug);
fieldRegistry
    .add("list.thing", { component: ThingField, supportedTypes: ["many2one"] })
    .add("kanban.thing", thingField);
viewRegistry.add("thing_view", { type: "form" }, { sequence: 5, force: true, label: "Thing" });
registry.category("fields").add(`thing_${"dyn"}`, thingField);
registry.category("fields").add("thing", ThingField);

if (odoo.debug) {
    registry.category("actions").add("thing_debug_action", () => {});
}

if (odoo.debug) {
    const registry = makeOther();
    registry.category("fields").add("shadowed", ThingField);
}

for (const thingField of [ThingField]) {
    registry.category("fields").add("looped", thingField);
}

legacyFields.add("legacy_thing", ThingField);

function later() {
    registry.category("actions").add("ignored", () => {});
}

const thingService = {
    dependencies: ["orm", "notification"],
    async: ["load"],
    start(env, { orm }) {
        return { load: () => orm.call("demo.thing", "load") };
    },
};
registry.category("services").add("thing", thingService);

const dialog = { add() {} };
dialog.add(ThingField, {});
