import { QueuedPrompt, queuePrompt, queuedPromptBody } from './chat';

describe('queuedPromptBody', () => {
  it('keeps the model selected when the message was queued', () => {
    // The controls can change while an earlier turn runs. The queued request
    // must still use the model that was selected before Send was pressed.
    const queued = queuePrompt(
      'Build the Studio Board',
      [],
      'code',
      'openai/gpt-5.6-sol',
    );

    expect(queuedPromptBody(queued)).toEqual({
      message: 'Build the Studio Board',
      agent: 'code',
      model: 'openai/gpt-5.6-sol',
    });
  });

  it('leaves model out when the default was selected', () => {
    const queued: QueuedPrompt = queuePrompt('Use the project default', [], 'code', '');

    expect(queuedPromptBody(queued)).toEqual({
      message: 'Use the project default',
      agent: 'code',
    });
  });
});
