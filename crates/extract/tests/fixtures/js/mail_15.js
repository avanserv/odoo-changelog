/** @odoo-module **/

import {
    registerFieldPatchModel,
    registerIdentifyingFieldsPatch,
    registerInstancePatchModel,
    registerNewModel,
} from '@mail/model/model_core';
import { attr, one2many } from '@mail/model/model_field';

function factory(dependencies) {

    class Thread extends dependencies['mail.model'] {

        static convertData(data) {
            return { id: data.id };
        }

        async markAllAsRead() {
            await this.async(() => this.env.services.rpc({ route: '/mail/thread/read' }));
        }

        _computeHasActivities() {
            return this.activities.length > 0;
        }
    }

    Thread.fields = {
        activities: one2many('mail.activity', { inverse: 'thread' }),
        hasActivities: attr({ compute: '_computeHasActivities', default: false }),
        id: attr({ required: true }),
    };
    Thread.identifyingFields = ['model', 'id'];
    Thread.modelName = 'mail.thread';

    return Thread;
}

registerNewModel('mail.thread', factory);

registerInstancePatchModel('mail.partner', 'demo/static/src/models/partner/partner.js', {
    async checkIsEmployee() {
        await this.async(() => this.messaging.models['hr.employee'].performRpcSearchRead({}));
    },
});

registerFieldPatchModel('mail.partner', 'demo/static/src/models/partner/partner.js', {
    employee: attr(),
});

registerInstancePatchModel('mail.partner', 'demo_more', {
    openProfile() {},
});

registerIdentifyingFieldsPatch('mail.partner', 'demo', identifyingFields => {
    identifyingFields[0].push('employee');
});
