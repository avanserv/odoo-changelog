/** @odoo-module **/

import { registry } from "@web/core/registry";

export function before() {
    return 0;
}

export class Broken {
    first() {
        return 1;
    }

    broken() {
        if (this.x) {
            return 2;
    }

    last() {
        return 3;
    }
}

export const after = 1;
registry.category("fields").add("after", after);
