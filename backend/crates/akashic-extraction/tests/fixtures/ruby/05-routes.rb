Rails.application.routes.draw do
  # `to:` is the handler; `as:` is a named-route alias with a string value.
  # The handler must come from the `to:` pair, never from `as:`.
  get '/widgets', as: 'widgets_list', to: 'widgets#index'
  post '/widgets', to: 'widgets#create', as: 'create_widget'
end
