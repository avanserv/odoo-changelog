/** @odoo-module **/

import { registry } from "@web/core/registry";
import Thing, { helper as help, "odd name" as odd } from "./thing";
import * as utils from "../utils/index";
import "@demo/side_effect";
import { format } from "./format.js";

export function compute(a, b = 2, ...rest) {
    return help(a) + b + rest.length;
}

export async function* stream(source) {
    yield* source;
}

export const LIMIT = 80;
export let counter = { count: 0 };
const internal = utils.make(Thing);

export class Store extends utils.Base {
    static instances = 0;
    items = [];
    #secret = 1;

    add(item) {
        this.items.push(format(item));
    }

    get size() {
        return this.items.length;
    }
}

registry.category("demo_stores").add("store", Store);

export default { name: "demo", limit: LIMIT };
export { internal as shared, odd };
export * from "./z.js";
export * from "@demo/more";
export * as ns from "@demo/q";
export { thing as other } from "./thing";
