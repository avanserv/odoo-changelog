/** @odoo-module **/

import { registerModel, registerPatch } from '@mail/model/model_core';
import { attr, many, one } from '@mail/model/model_field';
import { clear } from '@mail/model/model_field_command';

registerModel({
    name: 'Thread',
    identifyingMode: 'xor',
    lifecycleHooks: {
        _willDelete() {
            this.messaging.browser.clearTimeout(this.timer);
        },
    },
    modelMethods: {
        convertData(data) {
            return { id: data.id, name: data.name };
        },
    },
    recordMethods: {
        async markAllAsRead() {
            await this.messaging.rpc({ route: '/mail/thread/read', params: { id: this.id } });
        },
        _computeHasActivities() {
            return this.activities.length > 0;
        },
    },
    fields: {
        activities: many('Activity', { inverse: 'thread' }),
        hasActivities: attr({ compute: '_computeHasActivities', default: false }),
        id: attr({ identifying: true }),
        name: attr({
            compute() {
                return this.displayName || clear();
            },
        }),
        partner: one('Partner'),
    },
    onChanges: [
        { dependencies: ['activities'], methodName: '_onChangeActivities' },
    ],
});

registerPatch({
    name: 'Activity',
    recordMethods: {
        async deleteServerRecord() {
            await this._super();
        },
    },
    fields: {
        calendarEventId: attr(),
    },
});
