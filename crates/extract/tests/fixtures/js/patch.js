/** @odoo-module **/

import { patch } from "@web/core/utils/patch";
import { ThingField } from "@demo/views/component";
import { ListRenderer } from "@web/views/list/list_renderer";

patch(ThingField.prototype, {
    setup() {
        super.setup(...arguments);
        this.extra = true;
    },
    get displayName() {
        return "patched";
    },
});

patch(ListRenderer.prototype, "demo.ListRenderer", {
    onCellClicked(record, column) {
        this._super(...arguments);
    },
});

class LocalThing {
    run() {}
}
patch(LocalThing.prototype, { run() { return 1; } });

patch(ThingField, { extraProps: { type: Object, optional: true } });
patch(ThingField, { more: 1 });

const mixin = { m() {} };
patch(ListRenderer, mixin);
