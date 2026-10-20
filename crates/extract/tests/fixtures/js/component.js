/** @odoo-module **/

import { Component, useState, xml } from "@odoo/owl";
import { _t } from "@web/core/l10n/translation";
import { standardFieldProps } from "@web/views/fields/standard_field_props";
import { Dialog } from "@web/core/dialog/dialog";
import { ThingAutocomplete } from "../thing/autocomplete";

class ConfirmDialog extends Component {
    static template = "demo.ConfirmDialog";
    static components = { Dialog };
    static props = ["title?", "confirm", "close"];
}

export class ThingField extends Component {
    static template = "demo.ThingField";
    static components = { ThingAutocomplete, ConfirmDialog, Inline: InlineThing };
    static props = {
        ...standardFieldProps,
        placeholder: { type: String, optional: true },
        canCreate: { type: Boolean, optional: true },
    };
    static defaultProps = { canCreate: true };
    static SEARCH_LIMIT = 8;

    setup() {
        this.state = useState({ open: false });
    }

    get displayName() {
        return this.props.record.data[this.props.name] || _t("None");
    }

    async onCreate(name) {
        await this.props.record.save();
        return name;
    }

    static extractProps({ attrs }) {
        return { placeholder: attrs.placeholder };
    }
}

export class InlineThing extends Component {
    static template = xml`
        <div class="o_inline_thing">
            <t t-esc="props.label"/>
        </div>`;
    static props = { label: String };
}

export class ThingListField extends ThingField {
    static template = "demo.ThingListField";
}

export class Helper {
    static version = 2;
    run() {}
}
