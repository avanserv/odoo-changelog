/** @odoo-module **/

import { registry } from "@web/core/registry";
import { patch } from "@web/core/utils/patch";
import { getFixture } from "./utils";

export function makeTestEnv(config = {}) {
    return { config, fixture: getFixture() };
}

export const nextTick = () => new Promise((resolve) => setTimeout(resolve));

registry.category("services").add("demo_test", { start() {} });

patch(Object, "demo", { now() {} });
