odoo.define('demo/static/src/models/activity/activity.js', function (require) {
'use strict';

const { registerClassPatchModel, registerNewModel } = require('mail/static/src/model/model_core.js');
const { attr, many2one } = require('mail/static/src/model/model_field.js');

function factory(dependencies) {

    class Activity extends dependencies['mail.model'] {
        _computeIsCurrentPartnerAssignee() {
            return this.assignee === this.env.messaging.currentPartner;
        }
    }

    Activity.fields = {
        assignee: many2one('mail.partner'),
        isCurrentPartnerAssignee: attr({
            compute: '_computeIsCurrentPartnerAssignee',
            dependencies: ['assignee'],
        }),
    };

    return Activity;
}

registerNewModel('mail.activity', factory);

registerClassPatchModel('mail.activity', 'demo/static/src/models/activity/activity.js', {
    convertData(data) {
        return this._super(data);
    },
});

registerNewModel(modelName, factory);

});
