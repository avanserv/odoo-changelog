odoo.define('demo.ThingOwl', function (require) {
    "use strict";

    const { Component } = owl;
    const { useState } = owl.hooks;
    const Bar = require('demo.Bar');

    class ThingOwl extends Component {
        constructor() {
            super(...arguments);
            this.state = useState({ value: 0 });
        }
    }
    ThingOwl.template = 'demo.ThingOwl';
    ThingOwl.props = ['value', 'onChange'];
    ThingOwl.components = { Bar };
    ThingOwl.defaultProps = { value: 0 };

    return ThingOwl;
});
