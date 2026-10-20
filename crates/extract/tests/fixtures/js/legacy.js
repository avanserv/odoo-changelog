odoo.define('demo.ThingWidget', function (require) {
"use strict";

var core = require('web.core');
var Widget = require('web.Widget');
var { WidgetMixin } = require('web.mixins');
var FormController = require('web.FormController');
var fieldRegistry = require('web.field_registry');
var publicWidget = require('web.public.widget');
var _t = core._t;

var ThingWidget = Widget.extend(WidgetMixin, {
    template: 'demo.ThingWidget',
    events: _.extend({}, Widget.prototype.events, {
        'click .o_thing': '_onClick',
    }),
    custom_events: {
        thing_changed: '_onThingChanged',
    },
    init: function (parent, options) {
        this._super.apply(this, arguments);
        this.options = options || {};
    },
    _onClick: function (ev) {
        ev.preventDefault();
    },
});

FormController.include({
    _onThingChanged: function () {
        return _t("Changed");
    },
});

fieldRegistry.add('thing', ThingWidget).add('thing_list', ThingWidget);
core.action_registry.add('demo.thing_action', ThingWidget);

var options = $.extend({}, { a: 1 }, { b: 2 });

publicWidget.registry.thingSnippet = publicWidget.Widget.extend({
    selector: '.o_thing_snippet',
    start: function () {
        return this._super.apply(this, arguments);
    },
});
publicWidget.registry.thingSnippet.include({
    start: function () {
        return this._super.apply(this, arguments);
    },
});

return {
    ThingWidget: ThingWidget,
    options: options,
    format: function (v) { return String(v); },
};
});

odoo.define('demo.thing_utils', ['web.core'], function (require) {
"use strict";

var core = require('web.core');

function helper(x) {
    return core._t(x);
}

return helper;
});
